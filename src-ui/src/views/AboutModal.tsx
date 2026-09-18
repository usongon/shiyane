import { useEffect, useState } from "react";
import { Modal, theme as antdTheme, Typography } from "antd";
import { AudioOutlined } from "@ant-design/icons";
import { getVersion } from "@tauri-apps/api/app";

/** 应用内「关于」弹窗：由原生菜单栏 拾言 → 关于拾言 触发 */
export default function AboutModal({
  open,
  onClose,
}: {
  open: boolean;
  onClose: () => void;
}) {
  const { token } = antdTheme.useToken();
  const [appVersion, setAppVersion] = useState("");

  useEffect(() => {
    getVersion().then(setAppVersion).catch(() => setAppVersion(""));
  }, []);

  return (
    <Modal open={open} onCancel={onClose} footer={null} width={340} centered title="关于">
      <div
        style={{
          display: "flex",
          flexDirection: "column",
          alignItems: "center",
          gap: 6,
          padding: "8px 0 4px",
          textAlign: "center",
        }}
      >
        <div
          className="coming-icon"
          style={{ width: 44, height: 44, borderRadius: 12, marginBottom: 4 }}
        >
          <AudioOutlined style={{ fontSize: 20, color: "#fff" }} />
        </div>
        <div style={{ fontSize: 15, fontWeight: 600 }}>
          拾言 Shiyane
          {appVersion && (
            <span
              className="mono"
              style={{ marginLeft: 8, fontSize: 11, fontWeight: 400, color: token.colorTextTertiary }}
            >
              v{appVersion}
            </span>
          )}
        </div>
        <div style={{ fontSize: 12.5 }}>
          Crafted by <b>usong</b>
        </div>
        <div style={{ display: "flex", alignItems: "center", gap: 8, marginTop: 4 }}>
          <span style={{ fontSize: 12.5, color: token.colorTextSecondary }}>GitHub</span>
          <Typography.Text copyable={{ text: "https://github.com/usongon" }} style={{ fontSize: 12.5 }}>
            github.com/usongon
          </Typography.Text>
        </div>
        <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <span style={{ fontSize: 12.5, color: token.colorTextSecondary }}>邮箱</span>
          <Typography.Text copyable={{ text: "zdhuntero@gmail.com" }} style={{ fontSize: 12.5 }}>
            zdhuntero@gmail.com
          </Typography.Text>
        </div>
        <span className="mono" style={{ fontSize: 10.5, color: token.colorTextTertiary, marginTop: 6 }}>
          FSL-1.1-ALv2 License
        </span>
      </div>
    </Modal>
  );
}
