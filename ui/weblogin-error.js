document.getElementById("retryBtn").addEventListener("click", () => {
  window.__TAURI__.core.invoke("retry_web_login");
});
document.getElementById("closeBtn").addEventListener("click", () => {
  window.__TAURI__.core.invoke("close_web_login");
});
