-- Heron: this window is one chat.
--
-- Heron.app (the `heron` build, src/heron.rs) opens the main window with
-- `window` = "heron". The shell then shows the one Heron chat and nothing
-- else: no sidebar, session tabs, new chat, palette, settings or panel. The
-- chat is the one heron-keeper keeps on your computers: top-level, titled
-- "Heron", working in `~/heron` (apps/ios/Zeron/Heron/Heron.swift is the
-- same rule on the phone). It isn't this window's to make: until a computer
-- has made it, a line says what's happening.
--
--   heron.on        whether this is Heron's window
--   heron.step      "connecting" | "starting" | "open" (tracked)
--   heron.chat      the chat's id, once there is one (tracked)
--   heron.Status()  what shows in the chat's place until then
--   heron.ALLOWED   the window's commands Heron keeps (screens/shell/keys ids)

local ds = require("@ds")
local sessions = require("@account/sessions")

local M = {}

M.on = ui.shared("window", "onboarding"):peek() == "heron"

local TITLE = "Heron"
local function is_heron(c: sessions.Chat): boolean
	return c.title == TITLE and (c.cwd == "~/heron" or c.cwd == "~/heron/")
end

-- The oldest of `list` that is the Heron chat (the original, as the phone
-- picks it).
local function oldest(list: { sessions.Chat }): sessions.Chat?
	local best: sessions.Chat? = nil
	for _, c in list do
		if is_heron(c) and (best == nil or c.created < (best :: sessions.Chat).created) then
			best = c
		end
	end
	return best
end

-- The Heron chat: a live one first, else an archived one (brought back as
-- it's opened). `chats` holds only top-level chats that aren't archived.
local found = ui.computed(function(): { id: string, archived: boolean }?
	local now = sessions.now:get()
	if now.state ~= "ready" then
		return nil
	end
	local live = oldest(now.chats)
	if live then
		return { id = live.id, archived = false }
	end
	local old = oldest(now.archived or {})
	if old then
		return { id = old.id, archived = true }
	end
	return nil
end)

M.chat = ui.computed(function(): string?
	local f = found:get()
	return if f then f.id else nil
end)

M.step = ui.computed(function(): string
	if sessions.now:get().state ~= "ready" then
		return "connecting"
	end
	return if found:get() then "open" else "starting"
end)

-- An archived Heron chat is brought back, once each time it's found so.
local asked: string? = nil
ui.effect(function()
	local f = found:get()
	if f and f.archived and asked ~= f.id then
		asked = f.id
		sessions.unarchive(f.id)
	end
end)

-- Connecting for a while means Zeron isn't running on this Mac: Heron is
-- only a window onto it.
local slow = ui.state("heron_connecting_slow", false)
ui.effect(function()
	if M.step:get() == "connecting" then
		ui.after(6000, function()
			if M.step:peek() == "connecting" then
				slow:set(true)
			end
		end)
	else
		slow:set(false)
	end
end)

function M.Status(): Node
	return ui.column {
		id = "heron-status",
		w_full = true,
		h_full = true,
		min_w = 0,
		items = "center",
		justify = "center",
		p = "$space.4",
		bg = "$zeron.window",
		ds.EmptyState {
			icon = "widget",
			title = function(): string
				local step = M.step:get()
				if step == "connecting" then
					return if slow:get() then "Zeron isn't running on this Mac" else "Connecting…"
				end
				return "Starting Heron…"
			end :: any,
			body = function(): string
				local step = M.step:get()
				if step == "connecting" then
					return if slow:get()
						then "Heron is a window onto Zeron. Open Zeron, sign in, and Heron will pick up from there."
						else "Reaching Zeron on this Mac."
				end
				return "One of your computers is making the Heron chat. It shows here as soon as it exists."
			end :: any,
		},
	}
end

-- The window's commands Heron keeps: find in the chat, close the window,
-- reload the scripts. Everything else leads somewhere Heron doesn't have.
M.ALLOWED = {
	["shell.find"] = true,
	["shell.close"] = true,
	["shell.reload_interface"] = true,
	["shell.dictate"] = true,
}

return M
