// Per-run Zeron bridge for Pi's /tree. Pi's RPC has no navigate command, and
// only an extension command can reach ctx.navigateTree. No settings are modified.
export default function (pi) {
  const quote = (text) => {
    const flat = String(text).replace(/\s+/g, " ").trim();
    return `“${flat.length > 80 ? flat.slice(0, 80) + "…" : flat}”`;
  };
  const textOf = (content) =>
    typeof content === "string"
      ? content
      : (content ?? []).filter((block) => block.type === "text").map((block) => block.text).join(" ");

  pi.registerCommand("zeron-tree-jump", {
    description: "Zeron: move the conversation to an earlier point in the session tree",
    handler: async (args, ctx) => {
      const [id, mode] = args.trim().split(/\s+/);
      try {
        if (typeof ctx.navigateTree !== "function") {
          throw new Error("This version of Pi cannot move around the session tree. Update Pi.");
        }
        const entry = id && ctx.sessionManager.getEntry(id);
        if (!entry) throw new Error(`There is no entry ${id ?? ""} in this session.`);
        const summarize = mode === "summarize";
        const result = await ctx.navigateTree(id, { summarize });
        if (result.cancelled) {
          ctx.ui.notify("The jump was cancelled.", "warning");
          return;
        }
        // Pi keeps the leaf in memory and resumes at the last entry of the file.
        // Appending one entry at the new leaf makes the jump outlive this process.
        pi.appendEntry("zeron-tree-jump", { target: id });
        const message = entry.type === "message" ? entry.message : undefined;
        const where =
          message?.role === "user"
            ? `Went back to before ${quote(textOf(message.content))}. Continue from here, or edit and resend it.`
            : `Continuing from ${quote(message ? textOf(message.content) : entry.type)}.`;
        ctx.ui.notify(
          summarize ? `${where} A summary of the branch you left is now part of the context.` : where,
          "info",
        );
      } catch (error) {
        ctx.ui.notify(error instanceof Error ? error.message : String(error), "error");
      }
    },
  });
}
