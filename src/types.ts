export type SourceId = string;

export interface Track {
  id: string;
  source: SourceId;
  title: string;
  artist: string;
  album: string;
  artworkUrl: string;
  audioUrl: string;
  durationMs: number;
  format?: string;
  quality?: string;
  adapterPayload?: Record<string, unknown>;
}

export type DownloadStatus = "queued" | "downloading" | "completed" | "failed";

export interface DownloadItem {
  id: string;
  track: Track;
  status: DownloadStatus;
  progress: number;
  path?: string;
  error?: string;
}

export type ViewId = "discover" | "search" | "favorites" | "downloads" | "library" | "settings";
