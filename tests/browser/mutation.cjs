// Opt-in mutation probes alter only the browser's loopback script response.
// Production assets and server files are never modified. Normal runs do nothing.
const assert = require('node:assert/strict');

function scriptMutation(expected) {
  const mode = process.env.BIKESNEST_BROWSER_MUTANT;
  assert.ok(!mode || mode === expected, 'unexpected browser mutation mode');
  let applied = 0;
  return {
    async intercept(route) {
      if (!mode) return false;
      const pathname = new URL(route.request().url()).pathname;
      const file = mode === 'csrf-stale-head' ? 'auth' : 'app';
      if (!new RegExp('/' + file + '(?:\\.[^/.]+)?\\.js$').test(pathname)) return false;
      const response = await route.fetch();
      const original = await response.text();
      const needle = mode === 'csrf-stale-head'
        ? 'var token = input && input.value;'
        : 'if (intent && intent !== latestIntent) event.preventDefault();';
      const replacement = mode === 'csrf-stale-head'
        ? 'var token = document.querySelector(\'meta[name="csrf"]\').content;'
        : '/* mutation: stale search responses are no longer rejected */';
      assert.equal(original.split(needle).length - 1, 1, 'mutation anchor must occur exactly once');
      applied++;
      console.log('Applied browser-only mutation:', mode);
      await route.fulfill({ response, body: original.replace(needle, replacement) });
      return true;
    },
    assertApplied() {
      if (mode) assert.ok(applied > 0, 'the requested mutation must actually reach the browser');
    },
  };
}

module.exports = { scriptMutation };
