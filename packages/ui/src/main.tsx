import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import "./styles.css";
import { installLinkGuard } from "./lib/openExternal";

// 备注里的链接一律交给系统浏览器，不让 WebView 自己跳走。
installLinkGuard();
ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
