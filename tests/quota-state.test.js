const test = require("node:test");
const assert = require("node:assert/strict");
const Q = require("../ui/quota-state.js");

const now = Date.UTC(2026, 9, 30, 6, 0, 0);
const payload = { ok: true, fetchedAt: now / 1000 - 30, total: { usedRatio: 0.5, valueStatus: "valid" }, fiveHour: { usedRatio: 0.25, enabled: true, valueStatus: "valid" }, sevenDay: { usedRatio: 0.75, enabled: true, valueStatus: "valid" } };

test("endpoint uses one SVG rotation and handles exact quadrants", () => {
  for (const [ratio, x, y] of [[0, 66, 36], [0.25, 36, 66], [0.5, 6, 36], [0.75, 36, 6], [1, 66, 36]]) {
    const point = Q.endpoint(ratio);
    if (ratio !== 0) {
      assert.ok(Math.abs(point.x - x) < 1e-9);
      assert.ok(Math.abs(point.y - y) < 1e-9);
    }
    assert.equal(point.visible, ratio !== 0);
  }
  assert.equal(Q.endpoint(null).visible, false);
});

test("unknown, disabled and invalid metrics never become zero", () => {
  assert.equal(Q.orbRatio(null, "total", now), null);
  assert.equal(Q.orbRatio(payload, "missing", now), 0.5);
  assert.equal(Q.orbRatio({ ...payload, fiveHour: { usedRatio: null, valueStatus: "invalid" } }, "5h", now), null);
  assert.equal(Q.orbRatio({ ...payload, sevenDay: { usedRatio: 0, enabled: false, valueStatus: "valid" } }, "7d", now), null);
});

test("five-minute expiry hides orb value but snapshot remains renderable", () => {
  const old = { ...payload, fetchedAt: now / 1000 - 301 };
  assert.equal(Q.isExpired(old, now), true);
  assert.equal(Q.orbRatio(old, "total", now), null);
  assert.equal(Q.percent(old.total.usedRatio), "50.00%");
});

test("usage thresholds are exact", () => {
  assert.equal(Q.usageBand(0.4999), "normal");
  assert.equal(Q.usageBand(0.5), "warning");
  assert.equal(Q.usageBand(0.7999), "warning");
  assert.equal(Q.usageBand(0.8), "critical");
});

test("scheduler coalesces manual and automatic requests", async () => {
  let calls = 0;
  let release;
  const pending = new Promise((resolve) => { release = resolve; });
  const scheduler = new Q.Scheduler(() => { calls += 1; return pending; }, () => {}, () => true);
  const first = scheduler.refresh(false);
  const second = scheduler.refresh(true);
  assert.equal(first, second);
  release(payload);
  await Promise.all([first, second]);
  assert.equal(calls, 1);
  scheduler.stop();
});
