import { describe, expect, it } from "vitest";
import { answerRevealPath } from "./answerActions";

describe("answerRevealPath", () => {
	it("finds an absolute path in an answer", () => {
		const text = "Archive created:\n/home/sab/Pictures/BG/compressed_images.zip\nDone.";
		expect(answerRevealPath(text)).toBe("/home/sab/Pictures/BG/compressed_images.zip");
	});

	it("finds a home-relative path", () => {
		expect(answerRevealPath("Saved to ~/Downloads/report.pdf")).toBe("~/Downloads/report.pdf");
	});

	it("trims trailing sentence punctuation", () => {
		expect(answerRevealPath("The file is at /tmp/out.txt.")).toBe("/tmp/out.txt");
	});

	it("picks the LAST path (the artifact just produced)", () => {
		const text = "Read /etc/config and wrote the result to ~/out/result.json";
		expect(answerRevealPath(text)).toBe("~/out/result.json");
	});

	it("returns null when there is no path", () => {
		expect(answerRevealPath("The images have been compressed and zipped.")).toBeNull();
		expect(answerRevealPath("")).toBeNull();
		expect(answerRevealPath(null)).toBeNull();
	});

	it("ignores protocol-relative URLs and bare roots", () => {
		expect(answerRevealPath("see //example.com/x")).toBeNull();
		expect(answerRevealPath("the / root")).toBeNull();
	});

	it("does not run a path together with following prose", () => {
		// The path stops at the space, not swallowing " and extract".
		expect(answerRevealPath("Open ~/Pictures/BG/out.zip and extract it")).toBe(
			"~/Pictures/BG/out.zip",
		);
	});

	// A plain text answer that only MENTIONS a path must NOT sprout an action —
	// the button appears only when the answer actually produced an artifact.
	it("ignores an extension-less path mention with no creation verb", () => {
		expect(answerRevealPath("The hosts config lives in /etc/hosts")).toBeNull();
		expect(answerRevealPath("Add your alias to ~/.bashrc")).toBeNull();
		expect(answerRevealPath("The endpoint is /api/users")).toBeNull();
	});

	it("still reveals an extensioned artifact even without a creation verb", () => {
		// A produced file is almost always extensioned — that alone is enough.
		expect(answerRevealPath("The report is at /tmp/summary.pdf")).toBe("/tmp/summary.pdf");
	});

	it("reveals an extension-less nested path when the answer says it produced it", () => {
		expect(answerRevealPath("Created the project at ~/dev/my-app")).toBe("~/dev/my-app");
	});

	it("returns null for a pure explanatory answer (no artifact)", () => {
		const text = "Ollama runs models locally. Point Lychi at it in Settings and pick a model.";
		expect(answerRevealPath(text)).toBeNull();
	});
});
