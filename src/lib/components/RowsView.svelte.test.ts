// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import RowsView from "./RowsView.svelte";

/**
 * The first component-render tests in this project.
 *
 * They exist because `RowsView` shipped a fatal render bug that no test could
 * see: `{#each}` was keyed on `row.title`, and a real `dnf search firefox`
 * returns three rows titled "Firefox" (three flatpak remotes), three "Nvidia
 * VAAPI driver" and two "Joplin". Svelte 5 treats a duplicate key as FATAL —
 * the render threw, the panel painted nothing, and the spinner ran forever
 * while the backend had already returned complete results in ~2.5s.
 *
 * It presented as a backend hang. It was chased in the wrong layer twice.
 *
 * `svelte/server` cannot catch this: it renders to a string and never runs the
 * client keyed-each reconciler. That was measured, not assumed — under SSR the
 * buggy keying passed every test. Hence jsdom.
 */

afterEach(cleanup);

/** A row in the shape the specta-generated `Row` type produces. */
function row(title: string, extra: Record<string, unknown> = {}) {
	return { title, actions: [], accessories: [], ...extra };
}

describe("RowsView — duplicate keys (the bug that shipped)", () => {
	it("renders repeated titles instead of throwing", () => {
		// Verbatim from the failing case: three identical titles in one section.
		const sections = [
			{
				title: "flatpak",
				handler: "packages",
				rows: [row("Firefox"), row("Firefox"), row("Firefox")],
			},
		];

		expect(() => render(RowsView, { props: { sections } })).not.toThrow();
		expect(screen.getAllByText("Firefox")).toHaveLength(3);
	});

	it("renders duplicates that span different sections", () => {
		// The repo/flatpak split is the normal case for a package search, and
		// the same package legitimately appears in both.
		const sections = [
			{ title: "dnf", handler: "packages", rows: [row("Joplin")] },
			{ title: "flatpak", handler: "packages", rows: [row("Joplin")] },
		];

		expect(() => render(RowsView, { props: { sections } })).not.toThrow();
		expect(screen.getAllByText("Joplin")).toHaveLength(2);
	});

	it("renders sections that share a title", () => {
		// Sections were keyed on `section.title ?? "_"`, which collides just as
		// readily — two untitled sections produced the identical key twice.
		const sections = [
			{ handler: "packages", rows: [row("a")] },
			{ handler: "packages", rows: [row("b")] },
		];

		expect(() => render(RowsView, { props: { sections } })).not.toThrow();
		expect(screen.getByText("a")).toBeInTheDocument();
		expect(screen.getByText("b")).toBeInTheDocument();
	});
});

describe("RowsView — row content", () => {
	it("shows the empty state rather than a blank card", () => {
		render(RowsView, { props: { sections: [{ handler: "packages", rows: [] }] } });
		expect(screen.getByText("Nothing to show")).toBeInTheDocument();
	});

	it("renders subtitle, badge and accessories", () => {
		const sections = [
			{
				handler: "snippets",
				rows: [
					row("email-intro", {
						subtitle: "Hello there,",
						badge: { text: "done", tone: "ok" },
						accessories: [{ kind: "text", value: "42 chars" }],
					}),
				],
			},
		];
		render(RowsView, { props: { sections } });
		expect(screen.getByText("email-intro")).toBeInTheDocument();
		expect(screen.getByText("Hello there,")).toBeInTheDocument();
		expect(screen.getByText("done")).toBeInTheDocument();
		expect(screen.getByText("42 chars")).toBeInTheDocument();
	});

	it("a row with actions is a real button; one without is not", () => {
		// Not cosmetic: keyboard activation, focus and screen-reader semantics
		// come from the element being a <button>, which is why the component
		// writes both branches out rather than using <svelte:element>.
		const sections = [
			{
				handler: "ssh",
				rows: [
					row("prod", { actions: [{ id: "connect", label: "Connect", target: "prod" }] }),
					row("no-actions"),
				],
			},
		];
		render(RowsView, { props: { sections } });
		expect(screen.getByRole("button", { name: /prod/ })).toBeInTheDocument();
		expect(screen.queryByRole("button", { name: /no-actions/ })).not.toBeInTheDocument();
	});
});

describe("RowsView — actions", () => {
	it("invokes the producing handler, not a command string", () => {
		// The row-action contract: the frontend sends (handler, id, target) and
		// the backend resolves it. A row can never carry a command to run, which
		// is what keeps the action channel from being an execution channel.
		const onaction = vi.fn();
		const sections = [
			{
				handler: "packages",
				rows: [
					row("firefox", {
						actions: [{ id: "install", label: "Install", target: "firefox" }],
					}),
				],
			},
		];
		render(RowsView, { props: { sections, onaction } });

		screen.getByRole("button", { name: /firefox/ }).click();
		expect(onaction).toHaveBeenCalledWith("packages", "install", "firefox");
	});

	it("uses the FIRST action as the row default", () => {
		const onaction = vi.fn();
		const sections = [
			{
				handler: "todos",
				rows: [
					row("buy milk", {
						actions: [
							{ id: "toggle", label: "Mark done", target: "abc" },
							{ id: "delete", label: "Delete", target: "abc" },
						],
					}),
				],
			},
		];
		render(RowsView, { props: { sections, onaction } });

		screen.getByRole("button", { name: /buy milk/ }).click();
		// Enter takes the primary action; Delete must not be reachable by
		// activating the row itself.
		expect(onaction).toHaveBeenCalledWith("todos", "toggle", "abc");
		expect(onaction).toHaveBeenCalledTimes(1);
	});
});
