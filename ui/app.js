/* Kimi Quota Bar — card renderer.
   The card polls the native side via invoke("get_quota") every 60 s. */

function usageColor(ratio) {
  if (ratio == null) return "#64748b";
  if (ratio >= 0.8) return "#f87171";
  if (ratio >= 0.5) return "#fbbf24";
  return "#34d399";
}

function setRing(id, ratio) {
  const bar = document.getElementById("ring-" + id);
  const pct = document.getElementById("pct-" + id);
  if (!bar || !pct) return;
  const r = Number(bar.getAttribute("r")) || 20;
  const c = 2 * Math.PI * r;
  bar.style.strokeDasharray = c;
  if (ratio == null) {
    bar.style.strokeDashoffset = c;
    pct.textContent = "–";
    return;
  }
  const clamped = Math.max(0, Math.min(1, ratio));
  bar.style.strokeDashoffset = c * (1 - clamped);
  bar.style.stroke = usageColor(clamped);
  pct.textContent = (clamped * 100).toFixed(2) + "%";
}

function setRowDisabled(id, disabled) {
  const row = document.getElementById("ring-" + id).closest(".row");
  row.classList.toggle("disabled", disabled);
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

  const five = payload.fiveHour || {};
  const seven = payload.sevenDay || {};
  const fiveOff = five.enabled === false;
  const sevenOff = seven.enabled === false;

  setRing("total", payload.total && payload.total.usedRatio);
  setRing("ball", payload.total && payload.total.usedRatio);
  setRing("5h", fiveOff ? null : five.usedRatio);
  setRing("7d", sevenOff ? null : seven.usedRatio);
  setRowDisabled("5h", fiveOff);
  setRowDisabled("7d", sevenOff);

  document.getElementById("reset-total").textContent =
    fmtReset(payload.total && payload.total.resetTime, "date");
  document.getElementById("reset-5h").textContent = fiveOff
    ? "未启用"
    : fmtReset(five.resetTime, "rel");
  document.getElementById("reset-7d").textContent = sevenOff
    ? "未启用"
    : fmtReset(seven.resetTime, "rel");

  document.getElementById("updatedAt").textContent = payload.fetchedAt
    ? fmtUpdated(payload.fetchedAt)
    : "--";

  const code = payload.total && payload.total.codeRatio;
  footer.textContent =
    code != null
      ? `其中 Kimi Code 占 ${(code * 100).toFixed(2)}% · 每 60 秒自动刷新`
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

// ---------- login panel ----------

const CARD_H = 224;
const LOGIN_H = 400;
const invoke = (cmd, args) => window.__TAURI__.core.invoke(cmd, args);

let qrTimer = null;
let qrTicket = null;
let qrFailCount = 0;
let smsTimer = null;

function $(id) { return document.getElementById(id); }
function showEl(el) { el.classList.remove("hidden"); }
function hideEl(el) { el.classList.add("hidden"); }

async function openLogin() {
  showEl($("loginPanel"));
  await invoke("set_card_height", { height: LOGIN_H });
  startQrLogin();
}

async function closeLogin() {
  stopQrPolling();
  hideEl($("loginPanel"));
  await invoke("set_card_height", { height: CARD_H });
  refreshSessionUI();
  poll(); // refresh card (picks up a brand-new session right away)
}

function switchLoginTab(which) {
  $("tabQr").classList.toggle("active", which === "qr");
  $("tabSms").classList.toggle("active", which === "sms");
  $("qrPane").classList.toggle("hidden", which !== "qr");
  $("smsPane").classList.toggle("hidden", which !== "sms");
}

// --- QR login ---

function stopQrPolling() {
  if (qrTimer) { clearInterval(qrTimer); qrTimer = null; }
  qrTicket = null;
}

function setQrStatus(text) { $("qrStatus").textContent = text; }

function drawQr(matrix, size) {
  const canvas = $("qrCanvas");
  const ctx = canvas.getContext("2d");
  const quiet = 2;
  const total = size + quiet * 2;
  const scale = canvas.width / total;
  ctx.fillStyle = "#ffffff";
  ctx.fillRect(0, 0, canvas.width, canvas.height);
  ctx.fillStyle = "#16121f";
  const lines = matrix.trim().split("\n");
  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      if (lines[y] && lines[y][x] === "1") {
        ctx.fillRect((x + quiet) * scale, (y + quiet) * scale, scale, scale);
      }
    }
  }
}

async function startQrLogin() {
  stopQrPolling();
  qrFailCount = 0;
  hideEl($("qrRefreshBtn"));
  setQrStatus("正在生成二维码…");
  let r;
  try {
    r = await invoke("login_qr_create");
  } catch (e) {
    r = { ok: false, error: String(e) };
  }
  if (!r || r.ok !== true) {
    setQrStatus("生成失败：" + ((r && r.error) || "未知错误"));
    showEl($("qrRefreshBtn"));
    return;
  }
  qrTicket = r.code;
  drawQr(r.matrix, r.size);
  setQrStatus("等待扫码…");
  qrTimer = setInterval(pollQr, 2000);
}

async function pollQr() {
  if (!qrTicket) return;
  let r;
  try {
    r = await invoke("login_qr_poll", { code: qrTicket });
  } catch (e) {
    r = { ok: false, error: String(e) };
  }
  if (!r || r.ok !== true) {
    // Keep polling, but a persistent failure (e.g. session save error)
    // must surface instead of leaving the panel stuck on "等待扫码…".
    qrFailCount += 1;
    if (qrFailCount >= 5) {
      setQrStatus("连接异常：" + ((r && r.error) || "未知错误") + "（重试中…）");
    }
    return;
  }
  qrFailCount = 0;
  if (r.status === "STATUS_SCANNED") {
    setQrStatus("已扫码，请在手机上确认登录…");
  } else if (r.status === "STATUS_EXPIRED") {
    stopQrPolling();
    setQrStatus("二维码已过期");
    showEl($("qrRefreshBtn"));
  } else if (r.status === "STATUS_SUCCESS") {
    stopQrPolling();
    setQrStatus("登录成功！");
    await refreshSessionUI();
    setTimeout(closeLogin, 600);
  }
}

// --- SMS login ---

function setSmsError(text) { $("smsError").textContent = text || ""; }

function mapSmsSendError(err) {
  if (/CAPTCHA/i.test(err)) {
    return "服务端要求人机验证，请点下方「官方网页登录」完成短信验证";
  }
  return err;
}

async function sendSmsCode() {
  const cc = $("smsCC").value.trim().replace(/\D/g, "") || "86";
  const phone = $("smsPhone").value.trim();
  if (!/^\d{5,15}$/.test(phone)) {
    setSmsError("请输入正确的手机号");
    return;
  }
  $("smsSendBtn").disabled = true;
  setSmsError("");
  let r;
  try {
    r = await invoke("login_sms_send", { countryCode: cc, number: phone });
  } catch (e) {
    r = { ok: false, error: String(e) };
  }
  if (r && r.ok === true) {
    startSmsCountdown(60);
  } else {
    $("smsSendBtn").disabled = false;
    setSmsError(mapSmsSendError((r && r.error) || "发送失败"));
  }
}

function startSmsCountdown(secs) {
  if (smsTimer) clearInterval(smsTimer);
  let left = secs;
  const btn = $("smsSendBtn");
  btn.disabled = true;
  btn.textContent = `${left}s 后重发`;
  smsTimer = setInterval(() => {
    left -= 1;
    if (left <= 0) {
      clearInterval(smsTimer);
      smsTimer = null;
      btn.disabled = false;
      btn.textContent = "发送验证码";
    } else {
      btn.textContent = `${left}s 后重发`;
    }
  }, 1000);
}

async function submitSmsLogin() {
  const cc = $("smsCC").value.trim().replace(/\D/g, "") || "86";
  const phone = $("smsPhone").value.trim();
  const code = $("smsCode").value.trim();
  if (!/^\d{4,8}$/.test(code)) {
    setSmsError("请输入短信验证码");
    return;
  }
  $("smsLoginBtn").disabled = true;
  setSmsError("");
  let r;
  try {
    r = await invoke("login_sms_verify", { countryCode: cc, number: phone, verifyCode: code });
  } catch (e) {
    r = { ok: false, error: String(e) };
  }
  $("smsLoginBtn").disabled = false;
  if (r && r.ok === true) {
    await refreshSessionUI();
    closeLogin();
  } else {
    setSmsError((r && r.error) || "登录失败");
  }
}

// --- account state ---

async function refreshSessionUI() {
  let s = { ownSession: false };
  try {
    s = await invoke("session_status");
  } catch (e) { /* keep default */ }
  const loggedIn = !!(s && (s.ownSession || (s.followingDesktop && !s.loggedOut)));
  $("accountBtn").classList.toggle("active", loggedIn);
  return s;
}

// --- personal center ---

const PROFILE_H = 320;

const LEVEL_NAMES = {
  LEVEL_FREE: "免费版",
  LEVEL_BASIC: "基础会员",
  LEVEL_INTERMEDIATE: "中级会员",
  LEVEL_ADVANCED: "高级会员",
};
const SOURCE_NAMES = { qr: "扫码登录", sms: "手机验证码登录", web: "网页登录" };

function maskUid(uid) {
  if (!uid) return "未知用户";
  return uid.length > 12 ? uid.slice(0, 6) + "…" + uid.slice(-4) : uid;
}

function fmtDate(iso) {
  if (!iso) return "--";
  const t = new Date(iso);
  if (isNaN(t)) return "--";
  return `${t.getFullYear()}-${String(t.getMonth() + 1).padStart(2, "0")}-${String(t.getDate()).padStart(2, "0")}`;
}

async function openProfile() {
  showEl($("profilePanel"));
  await invoke("set_card_height", { height: PROFILE_H });
  $("profileUid").textContent = "加载中…";
  $("profileSource").textContent = "";
  let s;
  try {
    s = await invoke("get_profile");
  } catch (e) {
    s = { ok: false, error: String(e) };
  }
  const uid = s && s.userId;
  $("profileAvatar").textContent = (uid ? String(uid)[0] : "K").toUpperCase();
  $("profileUid").textContent = maskUid(uid);
  $("profileSource").textContent = s && s.ownSession
    ? (SOURCE_NAMES[s.source] || "本卡片登录")
    : "跟随 Kimi 桌面客户端";
  const m = s && s.membership;
  if (m && (m.plan || m.level)) {
    const level = LEVEL_NAMES[m.level] || m.level || "";
    $("profilePlan").textContent = (m.plan || "--") + (level ? " · " + level : "");
    $("profileStatus").textContent =
      m.active === true || m.status === "SUBSCRIPTION_STATUS_ACTIVE" ? "生效中" : (m.status || "--");
    $("profileExpire").textContent = fmtDate(m.periodEnd);
  } else {
    $("profilePlan").textContent = "--";
    $("profileStatus").textContent = "--";
    $("profileExpire").textContent = "--";
  }
}

async function closeProfile() {
  hideEl($("confirmMask"));
  hideEl($("profilePanel"));
  await invoke("set_card_height", { height: CARD_H });
  refreshSessionUI();
}

async function onAccountClick() {
  const s = await refreshSessionUI();
  const loggedIn = s && (s.ownSession || (s.followingDesktop && !s.loggedOut));
  if (loggedIn) {
    openProfile();
  } else {
    openLogin();
  }
}

// --- wiring ---

document.getElementById("refreshBtn").addEventListener("click", manualRefresh);
document.getElementById("retryBtn").addEventListener("click", manualRefresh);
document.getElementById("loginFromErrorBtn").addEventListener("click", openLogin);
document.getElementById("accountBtn").addEventListener("click", onAccountClick);
document.getElementById("loginCloseBtn").addEventListener("click", closeLogin);
document.getElementById("tabQr").addEventListener("click", () => switchLoginTab("qr"));
document.getElementById("tabSms").addEventListener("click", () => switchLoginTab("sms"));
document.getElementById("qrRefreshBtn").addEventListener("click", startQrLogin);
document.getElementById("smsSendBtn").addEventListener("click", sendSmsCode);
document.getElementById("smsLoginBtn").addEventListener("click", submitSmsLogin);
document.getElementById("webLoginBtn").addEventListener("click", () => invoke("open_web_login"));
document.getElementById("hideCardBtn").addEventListener("click", () => invoke("hide_card"));
document.getElementById("profileCloseBtn").addEventListener("click", closeProfile);
document.getElementById("logoutBtn").addEventListener("click", async () => {
  // 二次确认：文案区分自有会话与跟随桌面客户端两种登录态。
  const s = await refreshSessionUI();
  $("confirmText").textContent = s && s.ownSession
    ? "退出后卡片将显示未登录状态。"
    : "仅退出本卡片的登录，Kimi 桌面客户端不受影响。";
  showEl($("confirmMask"));
});
document.getElementById("confirmCancelBtn").addEventListener("click", () => hideEl($("confirmMask")));
document.getElementById("confirmOkBtn").addEventListener("click", async () => {
  hideEl($("confirmMask"));
  await invoke("logout");
  await refreshSessionUI();
  closeProfile();
  poll();
});
window.__kqbSession = refreshSessionUI;

// Drag the window from anywhere on the card except interactive controls.
document.querySelector(".card").addEventListener("mousedown", (e) => {
  if (e.button !== 0 || e.target.closest("button, input, canvas, .login-panel, .profile-panel, .confirm-mask")) return;
  window.__TAURI__.core.invoke("start_drag");
});

// --- ball mode (docked to the screen edge) ---

// Native side notifies via BOTH event emit and a direct eval of this
// function (eval survives the resize storm where events can get dropped).
window.__kqbBallMode = (on) => {
  document.body.classList.toggle("ball-mode", !!on);
  if (on) {
    // Fold panels WITHOUT closeLogin()/closeProfile(): those resize the
    // window via set_card_height, which would instantly undo ball mode.
    stopQrPolling();
    hideEl($("loginPanel"));
    hideEl($("profilePanel"));
    hideEl($("confirmMask"));
  }
};
window.__TAURI__.event.listen("ball-mode", (e) => window.__kqbBallMode(!!e.payload));

// Ball gestures: a plain click expands the card; a real drag moves the ball
// (dropping it away from the edge also expands, handled natively on Moved).
const ballEl = document.getElementById("ball");
let ballDownAt = null;
let ballDragging = false;
ballEl.addEventListener("mousedown", (e) => {
  if (e.button !== 0) return;
  ballDownAt = { x: e.clientX, y: e.clientY };
  ballDragging = false;
});
ballEl.addEventListener("mousemove", (e) => {
  if (!ballDownAt || ballDragging) return;
  if (Math.hypot(e.clientX - ballDownAt.x, e.clientY - ballDownAt.y) > 6) {
    ballDragging = true;
    invoke("start_drag");
  }
});
window.addEventListener("mouseup", () => {
  if (ballDownAt && !ballDragging) invoke("expand_card");
  ballDownAt = null;
  ballDragging = false;
});

// Surface any JS error on the card itself (debug aid).
window.addEventListener("error", (e) => {
  const footer = document.getElementById("footer");
  footer.textContent = "JS: " + e.message;
  footer.style.opacity = "0.9";
});

poll();
setInterval(poll, 60000);
refreshSessionUI();
