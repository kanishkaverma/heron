//! Enter steers a working agent; Option+Enter queues the message for after
//! the turn. Ways it can fail:
//! - Enter only queues (no steer RPC follows the queue write).
//! - The steer names the wrong row or misses the chat's host device.
//! - Option+Enter steers instead of leaving the row queued.
//! - Turning the preference off does not swap the two keys.
//! - A host that has not synced the new row yet answers `sent: false`, and the
//!   composer shows "Couldn't send that message" instead of retrying.
//! - Option+Enter on an idle chat does nothing instead of sending.
use super::*;
use zeron_rpc::{ClientFrame, ServerFrame, methods};

struct Engine {
    runtime: tokio::runtime::Runtime,
    requests: tokio::sync::mpsc::Receiver<String>,
    replies: tokio::sync::mpsc::Sender<String>,
}

impl Engine {
    fn pump(&self, cx: &mut gpui::TestAppContext) {
        for _ in 0..4 {
            self.runtime
                .block_on(async { tokio::task::yield_now().await });
            cx.run_until_parked();
        }
    }

    fn frames(&mut self, cx: &mut gpui::TestAppContext) -> Vec<ClientFrame> {
        self.pump(cx);
        std::iter::from_fn(|| self.requests.try_recv().ok())
            .map(|frame| serde_json::from_str(&frame).unwrap())
            .collect()
    }

    fn reply(&self, frame: &ClientFrame, value: serde_json::Value, cx: &mut gpui::TestAppContext) {
        self.replies
            .try_send(
                serde_json::to_string(&ServerFrame {
                    id: frame.id,
                    ok: Some(value),
                    ..Default::default()
                })
                .unwrap(),
            )
            .unwrap();
        self.pump(cx);
    }

    fn only(&mut self, method: &str, cx: &mut gpui::TestAppContext) -> ClientFrame {
        let frames: Vec<_> = self
            .frames(cx)
            .into_iter()
            .filter(|f| f.method.as_deref() == Some(method))
            .collect();
        assert_eq!(frames.len(), 1, "expected one {method}: {frames:?}");
        frames.into_iter().next().unwrap()
    }

    fn none(&mut self, method: &str, cx: &mut gpui::TestAppContext) {
        let frames = self.frames(cx);
        assert!(
            !frames.iter().any(|f| f.method.as_deref() == Some(method)),
            "unexpected {method}: {frames:?}"
        );
    }
}

fn press(
    handle: gpui::WindowHandle<Composer>,
    text: &str,
    keys: &str,
    busy: bool,
    cx: &mut gpui::TestAppContext,
) {
    handle
        .update(cx, |composer, _, cx| {
            composer.state.update(cx, |state, _| {
                if busy {
                    state.begin_pending_send("c", "running", chrono::Utc::now());
                } else {
                    state.end_pending_send("c", "running");
                }
            });
            composer
                .input
                .update(cx, |input, cx| input.set_text(text, cx));
            assert_eq!(
                composer.button_mode(cx),
                if busy {
                    SendButtonMode::Queue
                } else {
                    SendButtonMode::Send
                }
            );
        })
        .unwrap();
    cx.update_window(handle.into(), |_, window, cx| window.draw(cx).clear())
        .unwrap();
    cx.simulate_keystrokes(handle.into(), keys);
}

#[gpui::test]
fn busy_enter_steers_and_option_enter_queues(cx: &mut gpui::TestAppContext) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _guard = runtime.enter();
    let (_dir, handle) = super::tests::composer_focus_window(cx);
    cx.update(|cx| {
        crate::shell::apply_keymap(
            cx,
            &crate::settings::KeymapConfig::default(),
            ComposerSendBehavior::Enter,
        )
    });
    let (out, requests) = tokio::sync::mpsc::channel(64);
    let (replies, inbound) = tokio::sync::mpsc::channel(64);
    let queue_caps = [
        capabilities::MESSAGE_QUEUE_V1,
        capabilities::MESSAGE_QUEUE_ATTACHMENTS_V1,
        capabilities::MESSAGE_QUEUE_ACTIONS_V1,
    ];
    handle
        .update(cx, |composer, _, cx| {
            composer.state.update(cx, |state, _| {
                state.set_test_engine(
                    crate::state::EngineHandle::from_test_client(zeron_rpc::RpcClient::new(
                        out, inbound,
                    ))
                    .with_test_capabilities(&queue_caps),
                );
                state.devices = vec![
                    serde_json::from_value(serde_json::json!({
                        "id": "host", "name": "Host", "platform": "macos",
                        "capabilities": queue_caps,
                    }))
                    .unwrap(),
                ];
                state.chats = vec![
                    serde_json::from_value(serde_json::json!({
                        "id": "c", "deviceId": "host", "archived": false,
                        "cwd": "/work", "createdAt": chrono::Utc::now(),
                        "config": { "harness": "pi", "sandbox": "workspace-write" }
                    }))
                    .unwrap(),
                ];
                state.selected_chat = Some("c".into());
            });
        })
        .unwrap();
    let mut engine = Engine {
        runtime,
        requests,
        replies,
    };
    engine.frames(cx);
    let failure = |cx: &mut gpui::TestAppContext| {
        handle
            .read_with(cx, |composer, _| composer.failure.clone())
            .unwrap()
    };
    assert!(cx.update(|cx| crate::settings::current(cx).enter_steers_busy_agent));

    // Enter: queue, then steer that row on the chat's host. The first steer
    // races the row's sync to the host and is retried, not reported.
    press(handle, "steer me", "enter", true, cx);
    let queued = engine.only(methods::QUEUE_MESSAGE, cx);
    assert_eq!(queued.params["text"], "steer me");
    engine.reply(&queued, serde_json::json!({ "id": "q1" }), cx);
    let steer = engine.only(methods::STEER_QUEUED_MESSAGE_NOW, cx);
    assert_eq!(steer.params["id"], "q1");
    assert_eq!(steer.params["chatId"], "c");
    assert_eq!(steer.params["targetDeviceId"], "host");
    engine.reply(&steer, serde_json::json!({ "sent": false }), cx);
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(1));
    let retry = engine.only(methods::STEER_QUEUED_MESSAGE_NOW, cx);
    assert_eq!(retry.params["id"], "q1");
    engine.reply(&retry, serde_json::json!({ "sent": true }), cx);
    assert_eq!(failure(cx), None);

    // Option+Enter: queue only.
    press(handle, "queue me", "alt-enter", true, cx);
    let queued = engine.only(methods::QUEUE_MESSAGE, cx);
    assert_eq!(queued.params["text"], "queue me");
    engine.reply(&queued, serde_json::json!({ "id": "q2" }), cx);
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(5));
    engine.none(methods::STEER_QUEUED_MESSAGE_NOW, cx);

    // Preference off: the keys swap.
    cx.update(|cx| {
        crate::settings::update(crate::settings::SavePolicy::Immediate, cx, |s| {
            s.enter_steers_busy_agent = false;
        });
    });
    press(handle, "queue me too", "enter", true, cx);
    let queued = engine.only(methods::QUEUE_MESSAGE, cx);
    engine.reply(&queued, serde_json::json!({ "id": "q3" }), cx);
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(5));
    engine.none(methods::STEER_QUEUED_MESSAGE_NOW, cx);
    press(handle, "steer me too", "alt-enter", true, cx);
    let queued = engine.only(methods::QUEUE_MESSAGE, cx);
    engine.reply(&queued, serde_json::json!({ "id": "q4" }), cx);
    let steer = engine.only(methods::STEER_QUEUED_MESSAGE_NOW, cx);
    assert_eq!(steer.params["id"], "q4");
    engine.reply(&steer, serde_json::json!({ "sent": true }), cx);

    // Idle: Option+Enter is an ordinary send.
    press(handle, "idle", "alt-enter", false, cx);
    let run = engine.only(methods::QUEUE_COMMAND, cx);
    assert_eq!(run.params["command"]["request"]["prompt"], "idle");
    assert_eq!(failure(cx), None);
}
