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
 * The filesystem path an answer produced, or `null` if it names none.
 *
 * Picks the LAST absolute (`/…`) or home-relative (`~/…`) path in the text —
 * for a "created X" answer that is the artifact just made. Deliberately
 * conservative (only clear path shapes, trailing sentence punctuation trimmed)
 * so ordinary prose never sprouts an action.
 */
export function answerRevealPath(text: string | null | undefined): string | null {
	if (!text) return null;
	// A path token: ~ or /, then path chars, stopping at whitespace/quotes/parens
	// so a path embedded in prose isn't run together with the words around it.
	const re = /(?:^|[\s`'"(])((?:~|\/)[^\s`'")]+)/g;
	let last: string | null = null;
	for (const m of text.matchAll(re)) {
		// Trim sentence punctuation the model may have appended ("…/x.zip.").
		const p = m[1].replace(/[.,;:]+$/, "");
		// Ignore bare "/"/"~" and protocol-relative URLs ("//host/…").
		if (p.length > 2 && !p.startsWith("//")) last = p;
	}
	return last;
}
