import { useEffect, useState } from "react";

interface SegmentRange {
  start_id: number;
  end_id: number;
}

function App() {
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [segmentRange, setSegmentRange] = useState<SegmentRange | null>(null);

  useEffect(() => {
    fetch("/api/segments/range")
      .then((response) => {
        if (!response.ok) {
          throw new Error(`HTTP error! status: ${response.status}`);
        }
        return response.json();
      })
      .then((data: SegmentRange) => {
        setSegmentRange(data);
        setLoading(false);
      })
      .catch((err) => {
        console.error("Failed to load segment info:", err);
        setError(
          `Error loading segment information: ${err instanceof Error ? err.message : String(err)}`
        );
        setLoading(false);
      });
  }, []);

  if (loading) {
    return (
      <div id="app">
        <h1>Audio Stream Server</h1>
        <div id="loading">Loading segment information...</div>
      </div>
    );
  }

  if (error) {
    return (
      <div id="app">
        <h1>Audio Stream Server</h1>
        <div id="error" style={{ color: "red" }}>
          {error}
        </div>
      </div>
    );
  }

  if (!segmentRange) {
    return null;
  }

  const audioUrl = `/audio?start_id=${segmentRange.start_id}&end_id=${segmentRange.end_id}`;
  const mpdUrl = `/manifest.mpd?start_id=${segmentRange.start_id}&end_id=${segmentRange.end_id}`;

  return (
    <div id="app">
      <h1>Audio Stream Server</h1>
      <div id="content">
        <div className="url-section">
          <h2>Available URLs</h2>
          <div className="url-item">
            <h3>Audio URL (Ogg/Opus)</h3>
            <code>{audioUrl}</code>
          </div>
          <div className="url-item">
            <h3>MPD URL (DASH Manifest)</h3>
            <code>{mpdUrl}</code>
          </div>
        </div>
        <div className="info-section">
          <p>
            Segment Range:{" "}
            <span>
              {segmentRange.start_id} - {segmentRange.end_id}
            </span>
          </p>
        </div>
      </div>
    </div>
  );
}

export default App;
