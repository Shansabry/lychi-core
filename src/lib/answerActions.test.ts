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
});
