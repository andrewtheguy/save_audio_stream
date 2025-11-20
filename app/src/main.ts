import "./style.css";

interface SegmentRange {
  start_id: number;
  end_id: number;
}

async function loadSegmentInfo() {
  const loadingEl = document.getElementById("loading");
  const contentEl = document.getElementById("content");
  const errorEl = document.getElementById("error");
  const audioUrlEl = document.getElementById("audio-url");
  const mpdUrlEl = document.getElementById("mpd-url");
  const segmentRangeEl = document.getElementById("segment-range");

  try {
    const response = await fetch("/api/segments/range");
    if (!response.ok) {
      throw new Error(`HTTP error! status: ${response.status}`);
    }

    const data: SegmentRange = await response.json();

    const audioUrl = `/audio?start_id=${data.start_id}&end_id=${data.end_id}`;
    const mpdUrl = `/manifest.mpd?start_id=${data.start_id}&end_id=${data.end_id}`;

    if (audioUrlEl) audioUrlEl.textContent = audioUrl;
    if (mpdUrlEl) mpdUrlEl.textContent = mpdUrl;
    if (segmentRangeEl) segmentRangeEl.textContent = `${data.start_id} - ${data.end_id}`;

    if (loadingEl) loadingEl.style.display = "none";
    if (contentEl) contentEl.style.display = "block";
  } catch (error) {
    console.error("Failed to load segment info:", error);
    if (loadingEl) loadingEl.style.display = "none";
    if (errorEl) {
      errorEl.textContent = `Error loading segment information: ${error instanceof Error ? error.message : String(error)}`;
      errorEl.style.display = "block";
    }
  }
}

loadSegmentInfo();
