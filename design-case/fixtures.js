(function () {
  const now = () => Math.floor(Date.now() / 1000);
  const reset = (seconds) => new Date(Date.now() + seconds * 1000).toISOString();
  function payload(total, options) {
    const opts = options || {};
    return {
      ok: true,
      fetchedAt: now() - (opts.age || 0),
      stale: Boolean(opts.stale),
      error: opts.error,
      total: { usedRatio: total, codeRatio: 0.186, resetTime: reset(86400 * 5), valueStatus: "valid", codeStatus: "valid" },
      fiveHour: { usedRatio: 0.2834, enabled: opts.disabled, resetTime: reset(8280), valueStatus: "valid" },
      sevenDay: { usedRatio: 0.612, enabled: true, resetTime: reset(280800), valueStatus: "valid" },
    };
  }
  const scenarios = {};
  [0, 0.25, 0.4999, 0.5, 0.75, 0.7999, 0.8, 0.9999, 1].forEach((value) => {
    scenarios[`${(value * 100).toFixed(value * 100 % 1 ? 2 : 0)}%`] = payload(value);
  });
  scenarios["无数据"] = { ok: false, error: "演示：尚未取得额度", errorCategory: "network" };
  scenarios["字段缺失"] = payload(null);
  scenarios["指标禁用"] = payload(0.42, { disabled: false });
  scenarios["登录失效"] = { ok: false, error: "演示：登录已过期，请重新登录", errorCategory: "auth" };
  scenarios["断网旧数据"] = payload(0.72, { stale: true, age: 45, error: "演示：网络暂不可用" });
  scenarios["超过五分钟"] = payload(0.82, { stale: true, age: 360, error: "演示：长时间未刷新" });
  scenarios["非法字段"] = { ...payload(null), total: { usedRatio: null, valueStatus: "out-of-range", valueError: "比例超出有效范围", codeRatio: 0.186, codeStatus: "valid", resetTime: reset(10000) } };
  scenarios["超长错误"] = { ok: false, error: `演示：${"服务暂不可用，请稍后重试。".repeat(12)}`, errorCategory: "service" };
  window.KQB_FIXTURES = scenarios;
})();
