/* Kimi Quota Bar — card renderer.
   The card polls the native side via invoke("get_quota") every 60 s. */

const CIRCUMFERENCE = 2 * Math.PI * 20; // r=20 → 125.66

function usageColor(ratio) {
  if (ratio == null) return "#64748b";
  if (ratio >= 0.8) return "#f87171";
  if (ratio >= 0.5) return "#fbbf24";
  return "#34d399";
}

function setRing(id, ratio) {
  const bar = document.getElementById("ring-" + id);
  const pct = document.getElementById("pct-" + id);
  if (ratio == null) {
    bar.style.strokeDashoffset = CIRCUMFERENCE;
    pct.textContent = "–";
    return;
  }
  const clamped = Math.max(0, Math.min(1, ratio));
  bar.style.strokeDashoffset = CIRCUMFERENCE * (1 - clamped);
  bar.style.stroke = usageColor(clamped);
  pct.textContent = Math.round(clamped * 100) + "%";
}

function fmtReset(iso, mode) {
  if (!iso) return "重置时间未知";
  const t = new Date(iso);
  if (isNaN(t)) return "重置时间未知";
  const diffMs = t.getTime() - Date.now();
  if (diffMs <= 0) return "即将重置…";
  const mins = Math.floor(diffMs / 60000);
  const hours = Math.floor(mins / 60);
  const days = Math.floor(hours / 24);
  if (mode === "date" || days > 6) {
    return `${t.getMonth() + 1}/${String(t.getDate()).padStart(2, "0")} 重置`;
  }
  if (days >= 1) return `${days} 天 ${hours % 24} 小时后重置`;
  if (hours >= 1) return `${hours} 小时 ${mins % 60} 分后重置`;
  return `${mins} 分钟后重置`;
}

function fmtUpdated(unixSecs) {
  const d = new Date(unixSecs * 1000);
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
}

function render(payload) {
  const dot = document.getElementById("statusDot");
  const rows = document.getElementById("rows");
  const errorBox = document.getElementById("errorBox");
  const footer = document.getElementById("footer");

  if (!payload || payload.ok !== true) {
    dot.classList.add("err");
    rows.classList.add("hidden");
    errorBox.classList.remove("hidden");
    document.getElementById("errorMsg").textContent =
      (payload && payload.error) || "未知错误";
    footer.textContent = "点击重试或稍候自动重试";
    return;
  }

  dot.classList.remove("err");
  errorBox.classList.add("hidden");
  rows.classList.remove("hidden");

  setRing("total", payload.total && payload.total.usedRatio);
  setRing("5h", payload.fiveHour && payload.fiveHour.usedRatio);
  setRing("7d", payload.sevenDay && payload.sevenDay.usedRatio);

  document.getElementById("reset-total").textContent =
    fmtReset(payload.total && payload.total.resetTime, "date");
  document.getElementById("reset-5h").textContent =
    fmtReset(payload.fiveHour && payload.fiveHour.resetTime, "rel");
  document.getElementById("reset-7d").textContent =
    fmtReset(payload.sevenDay && payload.sevenDay.resetTime, "rel");

  document.getElementById("updatedAt").textContent = payload.fetchedAt
    ? fmtUpdated(payload.fetchedAt)
    : "--";

  const code = payload.total && payload.total.codeRatio;
  footer.textContent =
    code != null
      ? `其中 Kimi Code 占 ${(code * 100).toFixed(1)}% · 每 60 秒自动刷新`
      : "每 60 秒自动刷新";
}

async function poll() {
  try {
    const payload = await window.__TAURI__.core.invoke("get_quota");
    render(payload);
  } catch (e) {
    render({ ok: false, error: "内部调用失败: " + String(e) });
  }
}
window.__kqbPoll = poll;

function manualRefresh() {
  const btn = document.getElementById("refreshBtn");
  btn.classList.add("spin");
  setTimeout(() => btn.classList.remove("spin"), 1200);
  poll();
}

document.getElementById("refreshBtn").addEventListener("click", manualRefresh);
document.getElementById("retryBtn").addEventListener("click", manualRefresh);

// Drag the window from anywhere on the card except interactive controls.
document.querySelector(".card").addEventListener("mousedown", (e) => {
  if (e.button !== 0 || e.target.closest("button")) return;
  window.__TAURI__.core.invoke("start_drag");
});

// Surface any JS error on the card itself (debug aid).
window.addEventListener("error", (e) => {
  const footer = document.getElementById("footer");
  footer.textContent = "JS: " + e.message;
  footer.style.opacity = "0.9";
});

poll();
setInterval(poll, 60000);
