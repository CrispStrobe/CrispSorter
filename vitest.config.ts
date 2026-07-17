import { defineConfig } from 'vitest/config';

// Standalone vitest config (not the SvelteKit vite.config) — unit tests for pure
// TS lib modules like the AIToolkit graft client. Node env: global fetch /
// FormData / File / Response.
export default defineConfig({
	test: {
		environment: 'node',
		include: ['src/lib/**/*.test.ts'],
	},
});
