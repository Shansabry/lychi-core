/**
 * Vitest setup, applied to every test file.
 *
 * Deliberately minimal: the jest-dom matchers are additive and cost nothing in
 * a node-environment test, so they can load unconditionally. Anything that
 * touches `document` must NOT go here — most tests in this project run without
 * a DOM and should stay that way.
 */
import "@testing-library/jest-dom/vitest";
