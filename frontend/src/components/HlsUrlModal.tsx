import { React } from "../../deps.ts";
const { useState } = React;

interface HlsUrlModalProps {
  url: string;
  onClose: () => void;
}

export function HlsUrlModal({ url, onClose }: HlsUrlModalProps) {
  const [copied, setCopied] = useState(false);

  const handleCopy = () => {
    navigator.clipboard.writeText(url);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  const handleBackdropClick = (e: React.MouseEvent) => {
    if (e.target === e.currentTarget) {
      onClose();
    }
  };

  return (
    <div className="modal-backdrop" onClick={handleBackdropClick}>
      <div className="modal-content">
        <div className="modal-header">
          <h3>HLS URL</h3>
          <button className="modal-close-btn" onClick={onClose} title="Close">
            &times;
          </button>
        </div>
        <div className="modal-body">
          <input
            type="text"
            className="hls-url-input"
            value={url}
            readOnly
            onClick={(e) => (e.target as HTMLInputElement).select()}
          />
        </div>
        <div className="modal-footer">
          <button className="copy-url-btn" onClick={handleCopy}>
            {copied ? "Copied!" : "Copy URL"}
          </button>
        </div>
      </div>
    </div>
  );
}
