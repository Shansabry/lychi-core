/**
 * Lightweight fuzzy scorer for frontend use (history, ghost text).
 * No dependencies — runs synchronously on small lists (≤500 entries).
 */

const WORD_SEPARATORS = new Set([" ", "-", "_", "/", "\\", "."]);

export function fuzzyScore(query: string, candidate: string): number | null {
	const q = query.toLowerCase();
	const c = candidate.toLowerCase();

	if (q.length === 0) return null;
	if (q === c) return q.length * 10; // exact match bonus

	let score = 0;
	let ci = 0; // candidate index
	let prevMatchIdx = -2; // track consecutive matches

	for (let qi = 0; qi < q.length; qi++) {
		const ch = q[qi];
		let found = false;
		while (ci < c.length) {
			if (c[ci] === ch) {
				// Base point
				score += 1;
				// Consecutive match bonus
				if (ci === prevMatchIdx + 1) score += 4;
				// Word boundary bonus (start of string or after separator)
				if (ci === 0 || WORD_SEPARATORS.has(c[ci - 1])) score += 2;
				prevMatchIdx = ci;
				ci++;
				found = true;
				break;
			}
			ci++;
		}
		if (!found) return null;
	}

	// Prefix bonus — keeps prefix matches ranked highest
	if (c.startsWith(q)) score += q.length * 2;

	return score;
}

export interface FuzzyMatch {
	value: string;
	score: number;
}

export function fuzzyRank(query: string, candidates: string[]): FuzzyMatch[] {
	const trimmed = query.trim();
	if (!trimmed) return [];

	const seen = new Set<string>();
	const results: FuzzyMatch[] = [];

	for (const candidate of candidates) {
		if (seen.has(candidate)) continue;
		seen.add(candidate);
		const score = fuzzyScore(trimmed, candidate);
		if (score !== null) {
			results.push({ value: candidate, score });
		}
	}

	results.sort((a, b) => b.score - a.score);
	return results;
}
