// Keep early module/resource failures visible even before React mounts.
function showStartupError(message) {
  const root = document.getElementById("root");
  if (!root || root.querySelector(".app")) return;
  root.textContent = "日格界面加载失败：" + message;
  root.style.cssText = "padding:32px;font:14px -apple-system,sans-serif;white-space:pre-wrap";
}
window.addEventListener("error", function (event) {
  showStartupError(event.message || "无法载入界面资源，请重新启动应用。");
}, true);
window.addEventListener("unhandledrejection", function (event) {
  showStartupError(event.reason && event.reason.message || String(event.reason));
});
document.addEventListener("securitypolicyviolation", function (event) {
  showStartupError("界面资源被安全策略阻止：" + event.violatedDirective);
});
