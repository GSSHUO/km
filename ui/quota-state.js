(function (root) {
  "use strict";

  const METRICS = {
    total: { key: "total", label: "月度额度" },
    "5h": { key: "fiveHour", label: "5小时窗口" },
    "7d": { key: "sevenDay", label: "7天窗口" },
  };

  function finiteRatio(value) {
    return typeof value === "number" && Number.isFinite(value) && value >= 0 && value <= 1 ? value : null;
  }

  function metricInfo(payload, metric) {
    const definition = METRICS[metric] || METRICS.total;
    const stat = payload && payload.ok === true ? payload[definition.key] : null;
    return { ...definition, stat: stat || null };
  }

  function snapshotAge(payload, now) {
    if (!payload || !Number.isFinite(payload.fetchedAt)) return Infinity;
    return Math.max(0, (now || Date.now()) / 1000 - payload.fetchedAt);
  }

  function isExpired(payload, now) {
    return snapshotAge(payload, now) > 300;
  }

  function orbRatio(payload, metric, now) {
    if (!payload || payload.ok !== true || isExpired(payload, now)) return null;
    const { stat } = metricInfo(payload, metric);
    if (!stat || stat.enabled === false || stat.valueStatus && stat.valueStatus !== "valid") return null;
    return finiteRatio(stat.usedRatio);
  }

  function usageBand(ratio) {
    if (finiteRatio(ratio) == null) return "unknown";
    if (ratio >= 0.8) return "critical";
    if (ratio >= 0.5) return "warning";
    return "normal";
  }

  function percent(ratio) {
    const valid = finiteRatio(ratio);
    return valid == null ? "—" : `${(valid * 100).toFixed(2)}%`;
  }

  function formatReset(iso, mode, now) {
    if (!iso) return "重置时间未知";
    const date = new Date(iso);
    if (Number.isNaN(date.getTime())) return "重置时间未知";
    const diff = date.getTime() - (now || Date.now());
    if (diff <= 0) return "等待更新";
    const minutes = Math.floor(diff / 60000);
    const hours = Math.floor(minutes / 60);
    const days = Math.floor(hours / 24);
    if (mode === "date" || days > 6) return `${date.getMonth() + 1}/${String(date.getDate()).padStart(2, "0")} 重置`;
    if (days >= 1) return `${days}天${hours % 24}小时后重置`;
    if (hours >= 1) return `${hours}小时${minutes % 60}分后重置`;
    return `${Math.max(1, minutes)}分钟后重置`;
  }

  function formatUpdated(seconds, withSeconds) {
    if (!Number.isFinite(seconds)) return "—";
    const date = new Date(seconds * 1000);
    const hh = String(date.getHours()).padStart(2, "0");
    const mm = String(date.getMinutes()).padStart(2, "0");
    const ss = String(date.getSeconds()).padStart(2, "0");
    return withSeconds ? `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, "0")}-${String(date.getDate()).padStart(2, "0")} ${hh}:${mm}:${ss}` : `${hh}:${mm}`;
  }

  function endpoint(ratio) {
    const valid = finiteRatio(ratio);
    if (valid == null || valid === 0) return { angle: 0, x: 36, y: 6, visible: false };
    const angle = valid * 360;
    const radians = angle * Math.PI / 180;
    return {
      angle,
      x: 36 + 30 * Math.cos(radians),
      y: 36 + 30 * Math.sin(radians),
      visible: true,
    };
  }

  class Scheduler {
    constructor(fetcher, onResult, isVisible) {
      this.fetcher = fetcher;
      this.onResult = onResult;
      this.isVisible = isVisible;
      this.timer = null;
      this.pending = null;
      this.failures = 0;
      this.lastSuccess = 0;
      this.visible = true;
    }

    start() {
      this.refresh(false);
    }

    setVisible(visible) {
      const wasVisible = this.visible;
      this.visible = Boolean(visible);
      if (this.visible && !wasVisible) {
        const shouldRefresh = Date.now() - this.lastSuccess >= 60000 || this.failures > 0;
        if (shouldRefresh) this.refresh(false);
      }
      this.schedule(this.visible ? this.delay() : 0);
    }

    delay() {
      if (!this.failures) return 60000;
      return [60000, 120000, 240000, 300000][Math.min(this.failures - 1, 3)];
    }

    schedule(delay) {
      if (this.timer) clearTimeout(this.timer);
      this.timer = null;
      if (!delay || !this.visible) return;
      this.timer = setTimeout(() => this.refresh(false), delay);
    }

    refresh(manual) {
      if (!this.isVisible() && !manual) return Promise.resolve(null);
      if (this.pending) return this.pending;
      this.schedule(0);
      this.pending = Promise.resolve().then(this.fetcher).then(async (result) => {
        if (result && result.ok === true && !result.stale) {
          this.failures = 0;
          this.lastSuccess = Date.now();
        } else {
          this.failures = Math.min(this.failures + 1, 4);
        }
        await this.onResult(result, manual);
        this.schedule(this.delay());
        return result;
      }).catch(async (error) => {
        this.failures = Math.min(this.failures + 1, 4);
        await this.onResult({ ok: false, error: String(error) }, manual);
        this.schedule(this.delay());
        return null;
      }).finally(() => {
        this.pending = null;
      });
      return this.pending;
    }

    stop() {
      this.schedule(0);
      this.visible = false;
    }
  }

  const api = { METRICS, finiteRatio, metricInfo, snapshotAge, isExpired, orbRatio, usageBand, percent, formatReset, formatUpdated, endpoint, Scheduler };
  root.KQBQuota = api;
  if (typeof module !== "undefined" && module.exports) module.exports = api;
})(typeof window !== "undefined" ? window : globalThis);
