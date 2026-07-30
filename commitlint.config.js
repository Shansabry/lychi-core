/**
 * Conventional Commits, tuned to what this repo already writes.
 *
 * The type list below is the set actually in use (26 feat, 10 fix, 7 ci, 5
 * chore, plus refactor/docs/test/perf/build/style/revert), not an aspirational
 * one — a linter that rejects the project's own history teaches people to pass
 * `--no-verify`, which is worse than having no linter.
 *
 * Two deliberate relaxations:
 *
 * - No `subject-case` rule. Real subjects here start with identifiers and paths
 *   (`fix(ai): resolve @-referenced documents`), and enforcing lower-case would
 *   mangle them.
 * - Generous body/footer line length. Several commits in this repo carry the
 *   measurement or reasoning behind a change — the thing that is genuinely hard
 *   to reconstruct later — and a 100-column wrap would push people to write
 *   less of it.
 */
export default {
	extends: ["@commitlint/config-conventional"],
	rules: {
		"type-enum": [
			2,
			"always",
			[
				"feat",
				"fix",
				"perf",
				"refactor",
				"docs",
				"test",
				"build",
				"ci",
				"chore",
				"style",
				"revert",
			],
		],
		// Scope is free-form: it names the area touched (ai, file-search, hotkey,
		// linux, webview…) and a fixed list would go stale as the codebase grows.
		"scope-case": [2, "always", "kebab-case"],
		"subject-case": [0],
		"subject-max-length": [2, "always", 72],
		"body-max-line-length": [0],
		"footer-max-line-length": [0],
	},
};
