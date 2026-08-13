/**
 * Deriving quick actions from an AI answer's text.
 *
 * The agent's answer is free-form prose ("… saved to /home/u/x.zip. You can
 * open it as needed."). Rather than have the model emit structured actions, we
 * detect the clear signals in its text — today, a filesystem path — and offer a
 * real action (Open folder) for it. Keeping the detector HERE, in one place,
 * means the `AiAnswer` chip and the launcher's Enter handler agree on what the
 * actionable path is: one decider, two consumers.
 */

/**
 * The filesystem path an answer PRODUCED, or `null` if it produced none.
 *
 * The action ("Open folder") must appear only when the answer actually created
 * an artifact — not merely when it MENTIONS a path. A text answer that says
 * "config lives in /etc/hosts" or references a route like "/api/users" is not
 * actionable, and used to sprout a spurious button because any `/…` token was
 * treated as an artifact.
 *
 * A path qualifies as an artifact when EITHER:
 *   - it has a file extension (`…/report.pdf`, `…/out.zip`) — a produced file is
 *     almost always extensioned, and that alone is a strong artifact signal; OR
 *   - the answer carries a creation/output verb ("saved / created / wrote /
 *     generated / downloaded …") AND the path is a nested directory (`~/a/b`).
 *
 * A bare extension-less mention with no creation verb — "config lives in
 * /etc/hosts", a route like "/api/users", "use ~/.bashrc" — is referential prose,
 * not an artifact, and returns `null` (no button).
 *
 * Picks the LAST qualifying path — for a "created X" answer that is the artifact
 * just made. Deliberately conservative so ordinary prose never sprouts an action.
 */
const PRODUCED_VERB =
	/\b(saved|wrote|written|created|generated|downloaded|exported|extracted|copied|moved|placed|stored|output|produced|renamed)\b/i;

export function answerRevealPath(text: string | null | undefined): string | null {
	if (!text) return null;
	const produced = PRODUCED_VERB.test(text);

	// A path token: ~ or /, then path chars, stopping at whitespace/quotes/parens
	// so a path embedded in prose isn't run together with the words around it.
	const re = /(?:^|[\s`'"(])((?:~|\/)[^\s`'")]+)/g;
	let last: string | null = null;
	for (const m of text.matchAll(re)) {
		// Trim sentence punctuation the model may have appended ("…/x.zip.").
		const p = m[1].replace(/[.,;:]+$/, "");
		// Ignore bare "/"/"~" and protocol-relative URLs ("//host/…").
		if (p.length <= 2 || p.startsWith("//")) continue;
		const body = p.replace(/^~/, "");
		const basename = body.slice(body.lastIndexOf("/") + 1);
		// A real extension: `name.ext`. The leading dot of a DOTFILE (`.bashrc`,
		// `.gitignore`) is a hidden-file marker, not an extension separator — so
		// require at least one non-dot char before the final `.ext`.
		const hasExtension = /[^.]\.[A-Za-z0-9]{1,8}$/.test(basename);
		const isNested = body.split("/").filter(Boolean).length >= 2;
		// Extensioned path is an artifact on its own; an extension-less path only
		// counts when the answer says it produced something AND the path is nested
		// (so a lone "/api" route or "~/.bashrc" mention never triggers).
		if (hasExtension || (produced && isNested)) last = p;
	}
	return last;
}
