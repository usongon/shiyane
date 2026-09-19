import { useEffect, useMemo, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { App as AntdApp, Button, ConfigProvider, Segmented, Tooltip } from "antd";
import { SettingOutlined } from "@ant-design/icons";
import { BackendContext, getBackend } from "./lib/backend";
import { freshTheme } from "./theme";
import type { AppConfig } from "./lib/types";
import FilePage from "./views/FilePage";
import RealtimePage from "./views/RealtimePage";
import OverlayPage from "./views/OverlayPage";
import AboutModal from "./views/AboutModal";
import SettingsDrawer from "./views/SettingsDrawer";
import "./styles.css";

type ViewKey = "file" | "realtime";

const SEG_OPTIONS = [
  { value: "file", label: "文件转字幕" },
  { value: "realtime", label: "实时字幕" },
];

const VIEW_STATUS: Record<ViewKey, string> = {
  file: "文件转字幕",
  realtime: "实时字幕",
};

export default function App() {
  // 检查是否是 overlay 窗口
  const isOverlay = useMemo(() => {
    if (typeof window === "undefined") return false;
    const params = new URLSearchParams(window.location.search);
    return params.get("window") === "overlay";
  }, []);

  const [view, setView] = useState<ViewKey>("file");
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [aboutOpen, setAboutOpen] = useState(false);
  const backend = useMemo(() => getBackend(), []);
  const [config, setConfig] = useState<AppConfig | null>(null);
  const [platform, setPlatform] = useState<"macos" | "windows">("macos");

  useEffect(() => {
    backend
      .getConfig()
      .then(setConfig)
      .catch(() => setConfig(null));
  }, [backend]);

  useEffect(() => {
    backend
      .getHostPlatform()
      .then(setPlatform)
      .catch(() => setPlatform("macos"));
  }, [backend]);

  // 原生菜单栏 拾言 → 关于拾言 触发
  useEffect(() => {
    if (backend.mocked) return;
    const unlisten = listen("open-about", () => setAboutOpen(true));
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [backend]);

  // Overlay 窗口渲染精简版
  if (isOverlay) {
    return (
      <BackendContext.Provider value={backend}>
        <OverlayPage />
      </BackendContext.Provider>
    );
  }

  const cfgItems = [
    { key: "ASR", ok: !!config && config.asr.api_key.length > 0 },
    { key: "翻译", ok: !!config && config.translate.api_key.length > 0 },
    { key: "OSS", ok: !!config && config.oss !== null },
  ];

  return (
    <ConfigProvider theme={freshTheme}>
      <BackendContext.Provider value={backend}>
        <AntdApp>
          <div style={{ height: "100vh", display: "flex", flexDirection: "column" }}>
            {/* macOS Overlay 标题栏 / Windows 原生标题栏下只渲染工具行（无拖拽区、无红绿灯留白） */}
            {platform === "macos" ? (
              <div className="titlebar" data-tauri-drag-region>
                <div className="titlebar-zone titlebar-left" data-tauri-drag-region />
                <Segmented
                  value={view}
                  onChange={(v) => setView(v as ViewKey)}
                  options={SEG_OPTIONS}
                  aria-label="主导航"
                />
                <div className="titlebar-zone titlebar-right" data-tauri-drag-region>
                  {backend.mocked && (
                    <span className="demo-pill mono">
                      <i />
                      DEMO
                    </span>
                  )}
                  <Tooltip title="设置">
                    <Button
                      type={settingsOpen ? "primary" : "text"}
                      shape="circle"
                      size="small"
                      icon={<SettingOutlined />}
                      aria-label="设置"
                      onClick={() => setSettingsOpen(true)}
                    />
                  </Tooltip>
                </div>
              </div>
            ) : (
              <div className="titlebar titlebar-win">
                <div className="titlebar-zone titlebar-left" />
                <Segmented
                  value={view}
                  onChange={(v) => setView(v as ViewKey)}
                  options={SEG_OPTIONS}
                  aria-label="主导航"
                />
                <div className="titlebar-zone titlebar-right">
                  {backend.mocked && (
                    <span className="demo-pill mono">
                      <i />
                      DEMO
                    </span>
                  )}
                  <Tooltip title="设置">
                    <Button
                      type={settingsOpen ? "primary" : "text"}
                      shape="circle"
                      size="small"
                      icon={<SettingOutlined />}
                      aria-label="设置"
                      onClick={() => setSettingsOpen(true)}
                    />
                  </Tooltip>
                </div>
              </div>
            )}

            <div className="app-body">
              {/* 两个主视图常驻挂载，保留状态；拖拽监听不因切页而丢失。
                  minWidth:0 必须有：flex 项默认 min-width:auto，长文件名
                  nowrap min-content 会把 wrapper 撑出窗口（按钮被挤出可视区） */}
              <div style={{ display: view === "file" ? "flex" : "none", flex: 1, minWidth: 0, minHeight: 0 }}>
                <FilePage active={view === "file"} />
              </div>
              <div style={{ display: view === "realtime" ? "flex" : "none", flex: 1, minWidth: 0, minHeight: 0 }}>
                <RealtimePage active={view === "realtime"} />
              </div>
            </div>

            <div className="statusbar">
              <span>{settingsOpen ? "设置" : VIEW_STATUS[view]}</span>
              <div className="statusbar-right">
                {cfgItems.map((c) => (
                  <span
                    key={c.key}
                    className={`cfg-dot mono ${c.ok ? "on" : ""}`}
                    role="button"
                    tabIndex={0}
                    title={c.ok ? `${c.key} 已配置` : `${c.key} 未配置，点击前往设置`}
                    onClick={() => setSettingsOpen(true)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter" || e.key === " ") setSettingsOpen(true);
                    }}
                  >
                    <i />
                    {c.key}
                  </span>
                ))}
              </div>
            </div>

            {/* Windows 无菜单栏，About 入口放设置抽屉；macOS 走原生菜单 */}
            <SettingsDrawer
              open={settingsOpen}
              onClose={() => setSettingsOpen(false)}
              onAbout={platform === "windows" ? () => setAboutOpen(true) : undefined}
            />
            <AboutModal open={aboutOpen} onClose={() => setAboutOpen(false)} />
          </div>
        </AntdApp>
      </BackendContext.Provider>
    </ConfigProvider>
  );
}
