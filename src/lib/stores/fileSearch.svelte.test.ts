// @vitest-environment jsdom
import { beforeEach, describe, expect, it } from "vitest";
import type { FileSearchBatch } from "$lib/ipc";
import { completions } from "./completions.svelte";
import { context } from "./context.svelte";

/**
 * D3: `applyFileSearchBatch` carries three rules that are invisible until they
 * break — the stale-id guard, REPLACE-not-append (the comment records that
 * appending caused duplicates), and dedup by `full_path`. None were tested.
 *
 * jsdom because the state update is deferred through `requestAnimationFrame`
 * so it never blocks a keystroke paint.
 */

/** Let the deferred rAF body run. */
const nextFrame = () => new Promise((r) => requestAnimationFrame(() => r(null)));

function batch(over: Partial<FileSearchBatch> = {}): FileSearchBatch {
	return {
		search_id: completions.fileSearchId,
		results: [],
		done: false,
		has_ignore_rules: false,
		...over,
	} as FileSearchBatch;
}

function file(label: string, full_path: string, score = 100) {
	return {
		label,
		full_path,
		score,
		is_dir: false,
		description: null,
		size_bytes: null,
		modified_secs: null,
	};
}

describe("completions.applyFileSearchBatch", () => {
	beforeEach(() => {
		completions.items = [];
		completions.index = -1;
		completions.searchDone = false;
		completions.filePathMap = new Map();
	});

	it("ignores a batch from a superseded search", async () => {
		completions.applyFileSearchBatch(
			batch({ search_id: completions.fileSearchId - 1, results: [file("old.txt", "/o")] }),
		);
		await nextFrame();
		expect(completions.items).toHaveLength(0);
	});

	/**
	 * Each batch is the FULL ranked snapshot, re-emitted as the index fills.
	 * Appending re-adds every path on every emit — the duplicate bug the
	 * comment at the dedup records.
	 */
	it("replaces rather than appends across batches", async () => {
		completions.applyFileSearchBatch(batch({ results: [file("a.txt", "/a")] }));
		await nextFrame();
		completions.applyFileSearchBatch(
			batch({ results: [file("a.txt", "/a"), file("b.txt", "/b")] }),
		);
		await nextFrame();

		expect(completions.items.map((i) => i.label)).toEqual(["a.txt", "b.txt"]);
	});

	/**
	 * The case dedup CANNOT paper over: a row in the first batch that the
	 * second no longer ranks. Appending keeps it (with dedup happily allowing
	 * it, since the path is unique); replacing drops it, which is correct —
	 * each batch is the full current answer.
	 */
	it("drops a row the newest batch no longer contains", async () => {
		completions.applyFileSearchBatch(
			batch({ results: [file("stale.txt", "/stale"), file("keep.txt", "/keep")] }),
		);
		await nextFrame();
		expect(completions.items).toHaveLength(2);

		completions.applyFileSearchBatch(batch({ results: [file("keep.txt", "/keep")] }));
		await nextFrame();

		expect(completions.items.map((i) => i.label)).toEqual(["keep.txt"]);
	});

	it("dedupes two rows that resolve to the same path", async () => {
		completions.applyFileSearchBatch(
			batch({ results: [file("same", "/one/x"), file("same-again", "/one/x")] }),
		);
		await nextFrame();
		expect(completions.items).toHaveLength(1);
	});

	it("preserves the backend's ranking instead of re-sorting", async () => {
		completions.applyFileSearchBatch(
			batch({
				results: [file("third", "/3", 10), file("first", "/1", 99), file("second", "/2", 50)],
			}),
		);
		await nextFrame();
		// Emitted order is the ranked order — the store must not reorder it.
		expect(completions.items.map((i) => i.label)).toEqual(["third", "first", "second"]);
	});

	it("keeps section headers verbatim and never dedupes them", async () => {
		const sep = { ...file("Recent", ""), description: "__separator__" };
		completions.applyFileSearchBatch(
			batch({ results: [sep, file("a.txt", "/a"), { ...sep, label: "Other" }] }),
		);
		await nextFrame();
		const seps = completions.items.filter((i) => i.icon_path === "__separator__");
		expect(seps).toHaveLength(2);
	});

	it("auto-selects the first selectable row, skipping a leading header", async () => {
		const sep = { ...file("Recent", ""), description: "__separator__" };
		completions.applyFileSearchBatch(batch({ results: [sep, file("a.txt", "/a")] }));
		await nextFrame();
		expect(completions.items[completions.index].label).toBe("a.txt");
	});

	it("caps the list at 20 rows", async () => {
		const many = Array.from({ length: 40 }, (_, i) => file(`f${i}`, `/f${i}`));
		completions.applyFileSearchBatch(batch({ results: many }));
		await nextFrame();
		expect(completions.items).toHaveLength(20);
	});

	it("marks the search done and reports a genuinely empty result", async () => {
		completions.applyFileSearchBatch(batch({ results: [], done: true }));
		await nextFrame();
		expect(completions.searchDone).toBe(true);
		expect(completions.atNoResults).toBe(true);
	});
});

/**
 * D3: `context.extractStale` pulls a sentinel row out of a result set and sets
 * the staleness indicator from it. Untested, and it runs on every completion.
 */
describe("context.extractStale", () => {
	const stale = {
		label: "ctx",
		icon_path: "__context_stale__",
		score: 0,
		description: "re-reading the project",
	};
	const normal = { label: "open firefox", icon_path: null, score: 100, description: null };

	it("removes the sentinel and raises the indicator", () => {
		const out = context.extractStale([stale, normal] as never);
		expect(out).toHaveLength(1);
		expect(out[0].label).toBe("open firefox");
		expect(context.stale).toBe(true);
		expect(context.staleHint).toBe("re-reading the project");
	});

	it("clears the indicator when no sentinel is present", () => {
		context.extractStale([stale] as never); // raise it first
		const out = context.extractStale([normal] as never);
		expect(out).toHaveLength(1);
		expect(context.stale).toBe(false);
		expect(context.staleHint).toBe("");
	});

	it("returns the same rows untouched when there is nothing to extract", () => {
		const rows = [normal] as never;
		expect(context.extractStale(rows)).toEqual(rows);
	});
});
