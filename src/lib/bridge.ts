import { invoke, isTauri } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import type { DownloadItem, Track } from "../types";
import { defaultSources } from "../data/sources";

function requireDesktop(): void {
  if (!isTauri()) {
    throw new Error("请通过 `pnpm tauri dev` 在 LiteMusicDL 桌面版中使用真实音乐源");
  }
}

/** All source access stays in the Rust adapters; the WebView never calls a source directly. */
export async function searchTracks(
  query: string,
  sources: string[] = defaultSources,
  limit = 20,
  page = 1
): Promise<Track[]> {
  requireDesktop();
  return invoke<Track[]>("search_tracks", { query: query.trim(), sources, limit, page });
}

export async function downloadTrack(track: Track, directory: string, id: string): Promise<string> {
  requireDesktop();
  return invoke<string>("download_track", { track, directory, id });
}

export async function cancelDownload(id: string): Promise<void> {
  requireDesktop();
  return invoke<void>("cancel_download", { id });
}

export async function deleteFile(path: string): Promise<void> {
  requireDesktop();
  return invoke<void>("delete_file", { path });
}

export async function resolveQualities(tracks: Track[]): Promise<Track[]> {
  requireDesktop();
  return invoke<Track[]>("resolve_qualities", { tracks });
}

export async function loadDownloadHistory(): Promise<DownloadItem[]> {
  requireDesktop();
  return invoke<DownloadItem[]>("load_download_history");
}

export async function saveDownloadHistory(records: DownloadItem[]): Promise<void> {
  requireDesktop();
  return invoke<void>("save_download_history", { records });
}

/** The native player fetches sources through Rust with the adapter's request headers.
 * Returns the resolved track (audioUrl replaced with the proxied `music://` URI and
 * quality/format filled in from the source) so the UI can reflect the real quality. */
export async function preparePlayback(track: Track): Promise<Track> {
  requireDesktop();
  return invoke<Track>("prepare_playback", { track });
}

export async function chooseDownloadDirectory(defaultPath?: string): Promise<string | null> {
  requireDesktop();
  const selection = await open({
    directory: true,
    multiple: false,
    defaultPath: defaultPath || undefined,
    title: "选择 LiteMusicDL 下载目录"
  });
  return typeof selection === "string" ? selection : null;
}

export async function chooseMusicDirectory(defaultPath?: string): Promise<string | null> {
  requireDesktop();
  const selection = await open({
    directory: true,
    multiple: false,
    defaultPath: defaultPath || undefined,
    title: "选择本地音乐文件夹"
  });
  return typeof selection === "string" ? selection : null;
}

export async function scanLocalMusic(directory: string): Promise<Track[]> {
  requireDesktop();
  return invoke<Track[]>("scan_local_music", { directory });
}

export async function fetchLyrics(track: Track): Promise<string | null> {
  requireDesktop();
  return invoke<string | null>("get_lyrics", { track });
}
