import { describe, expect, it } from "vitest";
import {
	atPartial,
	atToken,
	isEmailAt,
	parentAtPartial,
	parentSearchInput,
	parseSearchInput,
	searchScope,
	spliceAtToken,
} from "./modes";

describe("parseSearchInput", () => {
	it("splits folder/term at the last slash", () => {
		const p = parseSearchInput("Documents/Reports/q1");
		expect(p).toEqual({
			folder: "Documents/Reports",
			term: "q1",
			hasFolder: true,
			termHasSpace: false,
		});
	});

	it("treats a top-level query as term-only, no folder", () => {
		const p = parseSearchInput("invoice");
		expect(p.folder).toBe("");
		expect(p.term).toBe("invoice");
		expect(p.hasFolder).toBe(false);
	});

	it("allows spaces in the FOLDER but flags a space in the TERM", () => {
		// "My Documents/report" — space is in the folder, term is clean.
		expect(parseSearchInput("My Documents/report").termHasSpace).toBe(false);
		// "notes hello" — no folder, space is in the term → not a search input.
		expect(parseSearchInput("notes hello").termHasSpace).toBe(true);
	});

	it("handles a trailing slash (empty term, folder set)", () => {
		const p = parseSearchInput("Documents/");
		expect(p.folder).toBe("Documents");
		expect(p.term).toBe("");
		expect(p.hasFolder).toBe(true);
	});
});

describe("searchScope", () => {
	it("appends the folder to the base", () => {
		const p = parseSearchInput("Documents/q");
		expect(searchScope("/home/u", p)).toBe("/home/u/Documents");
	});
	it("scopes a top-level query to the base itself", () => {
		const p = parseSearchInput("q");
		expect(searchScope("/home/u", p)).toBe("/home/u");
	});
});

describe("parentSearchInput", () => {
	it("goes up one level, keeping the leading slash", () => {
		expect(parentSearchInput("a/b/c")).toBe("/a/b/");
	});
	it("strips a trailing slash before going up", () => {
		expect(parentSearchInput("a/b/")).toBe("/a/");
	});
	it("stops at the root", () => {
		expect(parentSearchInput("a")).toBe("/");
		expect(parentSearchInput("")).toBe("/");
	});
});

describe("atToken", () => {
	it("splits before / token / after around a mid-string @", () => {
		// "run @src/main.rs --check"  (@ at index 4)
		const v = "run @src/main.rs --check";
		const t = atToken(v, 4);
		expect(t.before).toBe("run ");
		expect(t.atPart).toBe("@src/main.rs");
		expect(t.after).toBe(" --check");
	});
	it("runs the token to end-of-input when no trailing space", () => {
		const v = "open @notes/todo";
		const t = atToken(v, 5);
		expect(t.atPart).toBe("@notes/todo");
		expect(t.after).toBe("");
	});
});

describe("spliceAtToken", () => {
	it("replaces the token payload, preserving surroundings", () => {
		const v = "run @sr --check";
		expect(spliceAtToken(v, 4, "src/main.rs")).toBe("run @src/main.rs --check");
	});
	it("works at the end of input", () => {
		expect(spliceAtToken("open @no", 5, "notes/")).toBe("open @notes/");
	});
});

describe("atPartial", () => {
	it("returns the text after @ within the token", () => {
		expect(atPartial("run @src/ma --x", 4)).toBe("src/ma");
		expect(atPartial("open @notes", 5)).toBe("notes");
	});
});

describe("parentAtPartial", () => {
	it("goes up one level with a trailing slash", () => {
		expect(parentAtPartial("Documents/Reports/")).toBe("Documents/");
		expect(parentAtPartial("Documents/Reports")).toBe("Documents/");
	});
	it("returns empty at the top level", () => {
		expect(parentAtPartial("Documents")).toBe("");
		expect(parentAtPartial("Documents/")).toBe("");
	});
});

describe("isEmailAt", () => {
	it("flags an @ with non-space text before it as an email", () => {
		expect(isEmailAt("mail user@host.com", 9)).toBe(true);
	});
	it("does not flag an @ after a space or at the start", () => {
		expect(isEmailAt("run @file", 4)).toBe(false);
		expect(isEmailAt("@file", 0)).toBe(false);
	});
});
