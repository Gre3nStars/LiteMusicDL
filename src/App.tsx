import {
  Album,
  AudioLines,
  Check,
  Clock3,
  Compass,
  Download,
  Ellipsis,
  FileMusic,
  Filter,
  FolderOpen,
  Heart,
  History,
  Library,
  ListMusic,
  LoaderCircle,
  Maximize2,
  MicVocal,
  Moon,
  Pause,
  Play,
  Plus,
  Radio,
  Repeat2,
  Search,
  Settings,
  Shuffle,
  SkipBack,
  SkipForward,
  SlidersHorizontal,
  Trash2,
  Volume2,
  X
} from "lucide-react";
import { FormEvent, useEffect, useMemo, useRef, useState } from "react";
import { allSources, defaultSources, getSourceMeta, localSource } from "./data/sources";
import { cancelDownload, chooseDownloadDirectory, chooseMusicDirectory, deleteFile, downloadTrack, fetchLyrics, loadDownloadHistory, preparePlayback, resolveQualities, revealInFolder, saveDownloadHistory, scanLocalMusic, searchTracks } from "./lib/bridge";
import type { DownloadItem, Track } from "./types";

type Page = "search" | "discover" | "favorites" | "downloads" | "local" | "history" | "settings";

const SEARCH_PAGE_SIZE = 20;

const navGroups = [
  {
    label: "探索",
    items: [
      { id: "search" as Page, label: "搜索", icon: Search },
      { id: "discover" as Page, label: "发现", icon: Compass },
      { id: "favorites" as Page, label: "收藏", icon: Heart },
      { id: "downloads" as Page, label: "下载", icon: Download }
    ]
  },
  {
    label: "音乐资料库",
    items: [
      { id: "local" as Page, label: "本地音乐", icon: FileMusic },
      { id: "history" as Page, label: "最近播放", icon: History }
    ]
  }
];

function readFavorites(): Track[] {
  try {
    return (JSON.parse(localStorage.getItem("litemusic:favorites") || "[]") as Track[])
      .filter((track) => allSources.includes(track.source) || track.source === localSource);
  } catch {
    return [];
  }
}

function readDownloadPath() {
  return localStorage.getItem("litemusicdl:downloadPath") || "";
}

function readHistory(): Track[] {
  try {
    return (JSON.parse(localStorage.getItem("litemusic:history") || "[]") as Track[])
      .filter((track) => allSources.includes(track.source) || track.source === localSource);
  } catch { return []; }
}

function readSelectedSources() {
  try {
    const saved = JSON.parse(localStorage.getItem("litemusicdl:selectedSources") || "[]") as string[];
    const available = saved.filter((source) => allSources.includes(source));
    return available.length ? available : defaultSources;
  } catch {
    return defaultSources;
  }
}

function readHiddenLocalTracks() {
  try {
    return (JSON.parse(localStorage.getItem("litemusicdl:hiddenLocalTracks") || "[]") as string[])
      .filter((id) => id.startsWith("local:"));
  } catch {
    return [];
  }
}

/** Backfill the source-resolved quality/format into a list entry (never the
 * ephemeral `music://` playback URL, which may be evicted). */
function applyResolved(
  original: Track,
  resolved: Track,
  update: (updater: (items: Track[]) => Track[]) => void
) {
  update((items) => items.map((item) =>
    item.id === original.id
      ? {
        ...item,
        quality: resolved.quality ?? item.quality,
        format: resolved.format ?? item.format,
      }
      : item
  ));
}

/** Render the quality column: prefer the source-reported quality, then the file
 * format as a fallback (e.g. local FLAC/MP3), otherwise an empty placeholder. */
function qualityOf(track: Track): string {
  if (track.quality && track.quality.trim()) return track.quality.trim();
  if (track.format && track.format.trim()) return track.format.trim().toUpperCase();
  return "";
}

function qualityTone(label: string): string {
  const value = label.toLowerCase();
  if (/(无损|母带|全景声|环绕|杜比|音效|hires|flac|alac|hi-res|sq|zq)/.test(value)) return "lossless";
  if (/(320|高品质|hq|exhigh|640|192)/.test(value)) return "hq";
  if (/(128|标准|低品质|lq|96|48)/.test(value)) return "standard";
  return "other";
}

/** Extract the on-disk path of a scanned local track held in adapterPayload. */
function localPathOf(track: Track): string | undefined {
  const value = track.adapterPayload?.localPath;
  return typeof value === "string" && value.trim() ? value : undefined;
}

/** Return `track` with its `adapterPayload.localPath` set to the on-disk file of
 * a completed download (matched by track id), so playback serves the local file
 * rather than re-resolving a possibly-dead upstream URL. */
function withLocalPath(track: Track, downloads: DownloadItem[]): Track {
  const done = downloads.find((item) => item.status === "completed" && item.track.id === track.id);
  if (!done?.path) return track;
  const path = track.adapterPayload?.localPath;
  if (typeof path === "string" && path.trim()) return track;
  return { ...track, adapterPayload: { ...track.adapterPayload, localPath: done.path } };
}

export default function App() {
  const [lightTheme, setLightTheme] = useState(() => localStorage.getItem("litemusic:theme") === "light");
  const [gradient, setGradient] = useState(() => localStorage.getItem("litemusic:gradient") === "on");
  const [page, setPage] = useState<Page>("search");
  const [query, setQuery] = useState("");
  const [searchedQuery, setSearchedQuery] = useState("");
  const [tracks, setTracks] = useState<Track[]>([]);
  const [loading, setLoading] = useState(false);
  const [loadingMore, setLoadingMore] = useState(false);
  const [resultPage, setResultPage] = useState(1);
  const [hasMore, setHasMore] = useState(true);
  const [error, setError] = useState("");
  const [playbackError, setPlaybackError] = useState("");
  const [notice, setNotice] = useState("");
  const [playbackPending, setPlaybackPending] = useState(false);
  useEffect(() => {
    if (!notice) return;
    const timer = setTimeout(() => setNotice(""), 4200);
    return () => clearTimeout(timer);
  }, [notice]);
  const [selectedSources, setSelectedSources] = useState<string[]>(readSelectedSources);
  const [favorites, setFavorites] = useState<Track[]>(readFavorites);
  const [downloads, setDownloads] = useState<DownloadItem[]>([]);
  const [downloadsReady, setDownloadsReady] = useState(false);
  const [currentTrack, setCurrentTrack] = useState<Track | null>(null);
  const [queue, setQueue] = useState<Track[]>([]);
  const [history, setHistory] = useState<Track[]>(readHistory);
  const [isPlaying, setIsPlaying] = useState(false);
  const [elapsed, setElapsed] = useState(0);
  const [duration, setDuration] = useState(0);
  const [buffering, setBuffering] = useState(false);
  const [volume, setVolume] = useState(.76);
  const [localQuery, setLocalQuery] = useState("");
  const [defaultDownloadPath, setDefaultDownloadPath] = useState(readDownloadPath);
  const [localDirectory, setLocalDirectory] = useState(() => localStorage.getItem("litemusicdl:localMusicDirectory") || "");
  const [localTracks, setLocalTracks] = useState<Track[]>([]);
  const [hiddenLocalTrackIds, setHiddenLocalTrackIds] = useState<string[]>(readHiddenLocalTracks);
  const [localScanning, setLocalScanning] = useState(false);
  const [localError, setLocalError] = useState("");
  const [lyrics, setLyrics] = useState("");
  const [lyricsLoading, setLyricsLoading] = useState(false);
  const [lyricsError, setLyricsError] = useState("");
  const [showLyrics, setShowLyrics] = useState(false);
  const audioRef = useRef<HTMLAudioElement>(null);
  const searchTokenRef = useRef(0);
  const lastProgressAtRef = useRef(Date.now());
  const localScanTokenRef = useRef(0);

  useEffect(() => localStorage.setItem("litemusic:favorites", JSON.stringify(favorites)), [favorites]);
  useEffect(() => localStorage.setItem("litemusic:history", JSON.stringify(history)), [history]);
  useEffect(() => localStorage.setItem("litemusicdl:downloadPath", defaultDownloadPath), [defaultDownloadPath]);
  useEffect(() => localStorage.setItem("litemusicdl:localMusicDirectory", localDirectory), [localDirectory]);
  useEffect(() => localStorage.setItem("litemusicdl:hiddenLocalTracks", JSON.stringify(hiddenLocalTrackIds)), [hiddenLocalTrackIds]);
  useEffect(() => localStorage.setItem("litemusicdl:selectedSources", JSON.stringify(selectedSources)), [selectedSources]);
  useEffect(() => localStorage.setItem("litemusic:theme", lightTheme ? "light" : "dark"), [lightTheme]);
  useEffect(() => localStorage.setItem("litemusic:gradient", gradient ? "on" : "off"), [gradient]);
  useEffect(() => {
    let cancelled = false;
    loadDownloadHistory()
      .then((records) => {
        if (!cancelled) setDownloads(records.map((item) => item.status === "downloading"
          ? { ...item, status: "failed", progress: 0, error: "应用关闭前下载未完成" }
          : item));
      })
      .catch(() => undefined)
      .finally(() => { if (!cancelled) setDownloadsReady(true); });
    return () => { cancelled = true; };
  }, []);
  useEffect(() => {
    if (downloadsReady) saveDownloadHistory(downloads).catch(() => undefined);
  }, [downloads, downloadsReady]);
  useEffect(() => {
    const audio = audioRef.current;
    if (!audio || !currentTrack?.audioUrl) return;
    audio.volume = volume;
    if (isPlaying) audio.play().catch(() => setIsPlaying(false));
    else audio.pause();
  }, [isPlaying, currentTrack, volume]);
  // Watchdog: if we think we're "playing" but the media isn't advancing (slow /
  // stalled/blocked source), stop pretending and surface a clear message instead
  // of letting the timer run silently. While the element is still loading its
  // first frame (readyState < HAVE_CURRENT_DATA) we keep waiting — a slow remote
  // stream must not be misreported as blocked before it has a chance to start.
  useEffect(() => {
    if (!isPlaying) return;
    const interval = setInterval(() => {
      const audio = audioRef.current;
      if (audio && audio.readyState < 2) return;
      if (Date.now() - lastProgressAtRef.current > 4000) {
        setBuffering(false);
        setIsPlaying(false);
        setPlaybackError("播放受阻：网络缓慢或音源无响应");
      }
    }, 1000);
    return () => clearInterval(interval);
  }, [isPlaying]);
  useEffect(() => {
    let cancelled = false;
    setLyrics("");
    setLyricsError("");
    if (!currentTrack) {
      setLyricsLoading(false);
      return () => { cancelled = true; };
    }
    setLyricsLoading(true);
    fetchLyrics(currentTrack)
      .then((value) => {
        if (!cancelled) setLyrics(value || "");
      })
      .catch((lyricError) => {
        if (!cancelled) setLyricsError(String(lyricError));
      })
      .finally(() => {
        if (!cancelled) setLyricsLoading(false);
      });
    return () => { cancelled = true; };
  }, [currentTrack]);
  const favoriteIds = useMemo(() => new Set(favorites.map((track) => track.id)), [favorites]);
  const visibleTracks = useMemo(() => tracks.filter((track) => selectedSources.includes(track.source)), [selectedSources, tracks]);
  const visibleLocalTracks = useMemo(() => localTracks.filter((track) => !hiddenLocalTrackIds.includes(track.id)), [hiddenLocalTrackIds, localTracks]);
  const shownLocalTracks = useMemo(() => {
    const q = localQuery.trim().toLowerCase();
    if (!q) return visibleLocalTracks;
    return visibleLocalTracks.filter((track) =>
      track.title.toLowerCase().includes(q)
      || track.artist.toLowerCase().includes(q)
      || track.album.toLowerCase().includes(q)
    );
  }, [localQuery, visibleLocalTracks]);
  const downloadedIds = useMemo(() => new Set(downloads.filter((item) => item.status === "completed").map((item) => item.track.id)), [downloads]);
  const inFlightIds = useMemo(() => new Set(downloads.filter((item) => item.status === "downloading").map((item) => item.track.id)), [downloads]);
  async function submitSearch(event?: FormEvent) {
    event?.preventDefault();
    if (!query.trim()) return;
    if (!selectedSources.length) {
      setError("请至少选择一个音乐源");
      return;
    }
    const token = ++searchTokenRef.current;
    setPage("search");
    setSearchedQuery(query.trim());
    setLoading(true);
    setResultPage(1);
    setHasMore(true);
    setError("");
    try {
      const result = await searchTracks(query, selectedSources, SEARCH_PAGE_SIZE, 1);
      if (token === searchTokenRef.current) {
        setTracks(result);
        refreshQualities(result, token);
      }
    } catch (searchError) {
      setError(String(searchError));
      setTracks([]);
    } finally {
      setLoading(false);
    }
  }

  async function loadMore() {
    if (!searchedQuery || loadingMore) return;
    setLoadingMore(true);
    const nextPage = resultPage + 1;
    try {
      const next = await searchTracks(searchedQuery, selectedSources, SEARCH_PAGE_SIZE, nextPage);
      setTracks((current) => [...current, ...next.filter((track) => !current.some((item) => item.id === track.id))]);
      setResultPage(nextPage);
      if (next.length < SEARCH_PAGE_SIZE) setHasMore(false);
      refreshQualities(next, searchTokenRef.current);
    } catch (loadError) { setError(String(loadError)); }
    finally { setLoadingMore(false); }
  }

  /** Backfill each search result with its *actual* playable quality so the card
   * matches what playback will resolve (lossless may be VIP-gated). */
  async function refreshQualities(list: Track[], token: number) {
    if (!list.length) return;
    try {
      const updated = await resolveQualities(list);
      if (token !== searchTokenRef.current) return;
      const byId = new Map(updated.map((track) => [track.id, track]));
      setTracks((current) => current.map((track) => {
        const resolved = byId.get(track.id);
        if (!resolved) return track;
        return {
          ...track,
          quality: resolved.quality ?? track.quality,
          format: resolved.format ?? track.format,
        };
      }));
    } catch { /* keep metadata quality if resolution fails */ }
  }

  async function scanLocalLibrary(directory = localDirectory) {
    const selectedDirectory = directory.trim();
    if (!selectedDirectory) return;
    const token = ++localScanTokenRef.current;
    setLocalScanning(true);
    setLocalError("");
    try {
      const scannedTracks = await scanLocalMusic(selectedDirectory);
      if (token === localScanTokenRef.current) setLocalTracks(scannedTracks);
    } catch (scanError) {
      if (token === localScanTokenRef.current) {
        setLocalTracks([]);
        setLocalError(String(scanError));
      }
    } finally {
      if (token === localScanTokenRef.current) setLocalScanning(false);
    }
  }

  async function selectLocalDirectory() {
    try {
      const directory = await chooseMusicDirectory(localDirectory);
      if (!directory) return;
      setLocalDirectory(directory);
      await scanLocalLibrary(directory);
    } catch (directoryError) {
      setLocalError(`目录选择失败: ${String(directoryError)}`);
    }
  }

  function hideLocalTrack(track: Track) {
    setHiddenLocalTrackIds((ids) => ids.includes(track.id) ? ids : [...ids, track.id]);
  }

  function stopSearch() { searchTokenRef.current += 1; setLoading(false); setLoadingMore(false); setError("搜索已停止"); }

  async function play(track: Track) {
    setPlaybackError("");
    setPlaybackPending(true);
    setNotice(`正在准备播放「${track.title}」…`);
    // If this track is already downloaded, serve the on-disk file instead of
    // resolving a (possibly rate-limited/expired) upstream URL again. This is
    // what lets a downloaded song keep playing even when the source's stream
    // address is no longer available.
    const playTrack = withLocalPath(track, downloads);
    let playable: Track;
    try {
      playable = await preparePlayback(playTrack);
    } catch (playError) {
      setPlaybackPending(false);
      const message = `无法准备播放：${String(playError)}`;
      setPlaybackError(message);
      setNotice(`播放失败：${String(playError)}`);
      return;
    }
    setPlaybackPending(false);
    setCurrentTrack(playable);
    setElapsed(0);
    setDuration(track.durationMs / 1000 || 0);
    lastProgressAtRef.current = Date.now();
    setIsPlaying(true);
    setQueue((items) => items.some((item) => item.id === playable.id) ? items : [playable, ...items]);
    setHistory((items) => [playable, ...items.filter((item) => item.id !== playable.id)].slice(0, 50));
    setNotice(`正在播放「${playable.title}」`);
    // The resolved quality/format from the source is now known; write it back so
    // every list that shows this track reflects the real quality column value.
    applyResolved(track, playable, setTracks);
    applyResolved(track, playable, setFavorites);
  }

  function togglePlay() {
    const next = !isPlaying;
    setIsPlaying(next);
    if (currentTrack) setNotice(next ? `正在播放「${currentTrack.title}」` : "已暂停");
  }

  function handleSeek(value: number) {
    setElapsed(value);
    if (audioRef.current) audioRef.current.currentTime = value;
    lastProgressAtRef.current = Date.now();
  }

  function playRelative(offset: number) {
    const index = Math.max(0, queue.findIndex((track) => track.id === currentTrack?.id));
    if (queue.length) play(queue[(index + offset + queue.length) % queue.length]);
  }

  function toggleFavorite(track: Track) {
    setFavorites((items) => items.some((item) => item.id === track.id)
      ? items.filter((item) => item.id !== track.id)
      : [track, ...items]);
  }

  async function startDownload(track: Track) {
    let directory: string | null;
    try {
      directory = await chooseDownloadDirectory(defaultDownloadPath);
    } catch (directoryError) {
      setError(`无法打开目录选择器: ${String(directoryError)}`);
      return;
    }
    if (!directory) return;
    const id = `${track.id}-${Date.now()}`;
    setDownloads((items) => [{ id, track, status: "downloading", progress: 28 }, ...items]);
    setNotice(`已开始下载「${track.title}」`);
    try {
      const path = await downloadTrack(track, directory, id);
      setDownloads((items) => items.map((item) => item.id === id
        ? { ...item, status: "completed", progress: 100, path }
        : item));
      setNotice(`下载完成「${track.title}」`);
    } catch (downloadError) {
      setDownloads((items) => items.map((item) => item.id === id
        ? { ...item, status: "failed", progress: 0, error: String(downloadError) }
        : item));
      setNotice(`下载失败「${track.title}」：${String(downloadError)}`);
    }
  }

  /** Batch-download a list of tracks to a single directory, skipping any that
   * are already downloaded (or currently downloading). */
  async function batchDownload(tracksToDownload: Track[]) {
    const pending = tracksToDownload.filter((track) => !downloadedIds.has(track.id) && !inFlightIds.has(track.id));
    const skipped = tracksToDownload.length - pending.length;
    if (!pending.length) {
      setNotice(tracksToDownload.length ? "这些歌曲已全部下载" : "没有可下载的歌曲");
      return;
    }
    let directory = defaultDownloadPath;
    if (!directory) {
      try {
        const picked = await chooseDownloadDirectory(defaultDownloadPath);
        if (!picked) return;
        directory = picked;
      } catch (directoryError) {
        setError(`无法打开目录选择器: ${String(directoryError)}`);
        return;
      }
    }
    let ok = 0;
    let fail = 0;
    setNotice(`已开始批量下载 ${pending.length} 首歌曲…`);
    for (const track of pending) {
      const id = `${track.id}-${Date.now()}`;
      setDownloads((items) => [{ id, track, status: "downloading", progress: 28 }, ...items]);
      try {
        const path = await downloadTrack(track, directory, id);
        setDownloads((items) => items.map((item) => item.id === id
          ? { ...item, status: "completed", progress: 100, path }
          : item));
        ok += 1;
      } catch (downloadError) {
        setDownloads((items) => items.map((item) => item.id === id
          ? { ...item, status: "failed", progress: 0, error: String(downloadError) }
          : item));
        fail += 1;
      }
    }
    setNotice(fail
      ? `批量下载完成：成功 ${ok} 首，失败 ${fail} 首`
      : `已批量下载 ${ok} 首歌曲${skipped ? `（跳过 ${skipped} 首已下载）` : ""}`);
  }

  async function stopDownload(id: string) {
    setNotice("正在停止下载…");
    try {
      await cancelDownload(id);
      setNotice("已停止下载");
    } catch {
      setNotice("该下载任务已结束");
    }
  }

  function removeDownload(id: string) {
    const item = downloads.find((entry) => entry.id === id);
    if (item?.status === "downloading") cancelDownload(id).catch(() => undefined);
    setDownloads((items) => items.filter((entry) => entry.id !== id));
    setNotice("已从下载列表移除");
  }

  async function deleteDownloadFile(id: string) {
    const item = downloads.find((entry) => entry.id === id);
    if (item?.status === "downloading") cancelDownload(id).catch(() => undefined);
    if (item?.path) {
      try {
        await deleteFile(item.path);
        setNotice("已删除本地文件");
      } catch (deleteError) {
        setNotice(`删除文件失败：${String(deleteError)}`);
        return;
      }
    }
    setDownloads((items) => items.filter((entry) => entry.id !== id));
  }

  async function revealTrackFile(track: Track) {
    const path = localPathOf(track);
    if (!path) {
      setNotice("无法确定该本地歌曲的文件路径");
      return;
    }
    try {
      await revealInFolder(path);
    } catch (revealError) {
      setNotice(`打开所在文件夹失败：${String(revealError)}`);
    }
  }

  async function revealDownloadFile(item: DownloadItem) {
    const path = item.path;
    if (!path) {
      setNotice("该下载没有可定位的本地文件");
      return;
    }
    try {
      await revealInFolder(path);
    } catch (revealError) {
      setNotice(`打开所在文件夹失败：${String(revealError)}`);
    }
  }

  return (
    <div className={`obsidian-app ${lightTheme ? "light-theme" : ""} ${gradient ? "gradient-bg" : ""}`}>
      <Sidebar page={page} onNavigate={setPage} onToggleTheme={() => setLightTheme((value) => !value)} />

      <main className="workspace">
        <header className="command-bar" data-tauri-drag-region>
          <form className="command-search" onSubmit={submitSearch}>
            <button className="command-submit" type="submit" aria-label="提交搜索"><Search size={20} /></button>
            <input value={query} onChange={(event) => setQuery(event.target.value)} aria-label="搜索音乐" />
            {query && <button type="button" onClick={() => setQuery("")} aria-label="清空搜索"><X size={16} /></button>}
            {loading && <button type="button" className="command-stop" onClick={stopSearch} aria-label="停止搜索"><X size={16} /></button>}
          </form>
        </header>

        {page === "search" && <SourceStrip
          selectedSources={selectedSources}
          onToggle={(source) => setSelectedSources((items) => items.includes(source) ? items.filter((item) => item !== source) : [...items, source])}
        />}

        {page === "search" && (
          <SearchPage
            query={searchedQuery}
            tracks={visibleTracks}
            loading={loading}
            error={error}
            favoriteIds={favoriteIds}
            currentTrackId={currentTrack?.id ?? ""}
            onPlay={play}
            onFavorite={toggleFavorite}
            onDownload={startDownload}
            onLoadMore={loadMore}
            loadingMore={loadingMore}
            canLoadMore={hasMore}
            downloadedIds={downloadedIds}
            inFlightIds={inFlightIds}
          />
        )}
        {page === "favorites" && (
          <LibraryPage title="收藏" subtitle={`${favorites.length} 首歌曲`} tracks={favorites} empty="把想再听的音乐留在这里。" favoriteIds={favoriteIds} onPlay={play} onFavorite={toggleFavorite} onDownload={startDownload} downloadedIds={downloadedIds} inFlightIds={inFlightIds} onDownloadAll={() => batchDownload(favorites)} onRevealFile={revealTrackFile} />
        )}
        {page === "downloads" && <DownloadsPage items={downloads} onStop={stopDownload} onRemove={removeDownload} onDeleteFile={deleteDownloadFile} onReveal={revealDownloadFile} onPlay={play} />}
        {page === "discover" && (
          <LibraryPage title="发现" subtitle="仅展示音源实时返回的音乐" tracks={tracks} empty="搜索后，真实结果会出现在这里。" favoriteIds={favoriteIds} onPlay={play} onFavorite={toggleFavorite} onDownload={startDownload} downloadedIds={downloadedIds} inFlightIds={inFlightIds} onDownloadAll={() => batchDownload(tracks)} />
        )}
        {page === "local" && <LocalMusicPage tracks={shownLocalTracks} directory={localDirectory} scanning={localScanning} error={localError} hiddenCount={hiddenLocalTrackIds.length} favoriteIds={favoriteIds} currentTrackId={currentTrack?.id ?? ""} onChooseDirectory={selectLocalDirectory} onRescan={() => scanLocalLibrary()} onRestoreHidden={() => setHiddenLocalTrackIds([])} onRemoveFromLibrary={hideLocalTrack} onRevealFile={revealTrackFile} onPlay={play} onFavorite={toggleFavorite} onDownload={startDownload} downloadedIds={downloadedIds} inFlightIds={inFlightIds} query={localQuery} onQueryChange={setLocalQuery} />}
        {page === "history" && <LibraryPage title="最近播放" subtitle="继续刚才的声音" tracks={history} empty="还没有播放记录。" favoriteIds={favoriteIds} onPlay={play} onFavorite={toggleFavorite} onDownload={startDownload} downloadedIds={downloadedIds} inFlightIds={inFlightIds} onDownloadAll={() => batchDownload(history)} onRevealFile={revealTrackFile} />}
        {page === "settings" && (
          <SettingsPage
            defaultDownloadPath={defaultDownloadPath}
            onChangeDownloadPath={setDefaultDownloadPath}
            gradient={gradient}
            onToggleGradient={() => setGradient((value) => !value)}
          />
        )}
      </main>

      <NowPlayingPanel
        track={currentTrack}
        error={playbackError}
        queue={queue}
        isPlaying={isPlaying}
        favorite={currentTrack ? favoriteIds.has(currentTrack.id) : false}
        elapsed={elapsed}
        duration={duration}
        onTogglePlay={togglePlay}
        onFavorite={() => currentTrack && toggleFavorite(currentTrack)}
        onPlay={play}
        onClearQueue={() => setQueue(currentTrack ? [currentTrack] : [])}
        onOpenLyrics={() => currentTrack && setShowLyrics(true)}
        pending={playbackPending}
        buffering={buffering}
      />

      <PlayerBar
        track={currentTrack}
        isPlaying={isPlaying}
        favorite={currentTrack ? favoriteIds.has(currentTrack.id) : false}
        elapsed={elapsed}
        duration={duration}
        volume={volume}
        onTogglePlay={togglePlay}
        onFavorite={() => currentTrack && toggleFavorite(currentTrack)}
        onPrevious={() => playRelative(-1)}
        onNext={() => playRelative(1)}
        onSeek={handleSeek}
        onVolume={setVolume}
        onOpenLyrics={() => currentTrack && setShowLyrics(true)}
        pending={playbackPending}
        buffering={buffering}
      />

      {showLyrics && currentTrack && (
        <LyricsOverlay
          track={currentTrack}
          lyrics={lyrics}
          loading={lyricsLoading}
          error={lyricsError}
          elapsed={elapsed}
          duration={duration}
          isPlaying={isPlaying}
          pending={playbackPending}
        buffering={buffering}
          onClose={() => setShowLyrics(false)}
          onTogglePlay={togglePlay}
          onPrevious={() => playRelative(-1)}
          onNext={() => playRelative(1)}
          onSeek={handleSeek}
        />
      )}

      <audio
        ref={audioRef}
        src={currentTrack?.audioUrl}
        onLoadedMetadata={(event) => setDuration(event.currentTarget.duration)}
        onTimeUpdate={(event) => { lastProgressAtRef.current = Date.now(); setBuffering(false); setElapsed(event.currentTarget.currentTime); }}
        onWaiting={() => { lastProgressAtRef.current = Date.now(); setBuffering(true); }}
        onStalled={() => { lastProgressAtRef.current = Date.now(); setBuffering(true); }}
        onPlaying={() => { lastProgressAtRef.current = Date.now(); setBuffering(false); }}
        onCanPlay={() => { lastProgressAtRef.current = Date.now(); setBuffering(false); }}
        onError={(event) => {
          setBuffering(false);
          const code = event.currentTarget.error?.code;
          setPlaybackError(`播放请求失败${code ? `（音频错误 ${code}）` : ""}`);
          setIsPlaying(false);
        }}
        onEnded={() => playRelative(1)}
      />

      {notice && <div className="notice-toast" role="status">{notice}</div>}
    </div>
  );
}

function SourceStrip({ selectedSources, onToggle }: { selectedSources: string[]; onToggle: (source: string) => void }) {
  return <div className="source-strip"><div className="source-selection"><span className="source-strip-label"><Filter size={14} /> 搜索来源</span><div className="source-strip-options">{allSources.map((source) => { const meta = getSourceMeta(source); const selected = selectedSources.includes(source); return <button key={source} className={selected ? "selected" : ""} onClick={() => onToggle(source)} aria-pressed={selected}><i style={{ background: meta.color }} /><span>{meta.label}</span><b>{selected ? "✓" : ""}</b></button>; })}</div></div></div>;
}

function Sidebar({ page, onNavigate, onToggleTheme }: { page: Page; onNavigate: (page: Page) => void; onToggleTheme: () => void }) {
  return (
    <aside className="sidebar">
      <div className="brand"><span><AudioLines size={18} /></span><span><strong>LiteMusicDL</strong><small>音乐下载器</small></span></div>
      <nav>
        {navGroups.map((group) => (
          <div className="nav-group" key={group.label}>
            <p>{group.label}</p>
            {group.items.map((item) => {
              const Icon = item.icon;
              return <button key={item.id} className={page === item.id ? "active" : ""} onClick={() => onNavigate(item.id)}><i className="nav-ic"><Icon size={16} /></i><span>{item.label}</span></button>;
            })}
          </div>
        ))}
      </nav>
      <div className="sidebar-bottom">
        <button className={page === "settings" ? "active" : ""} onClick={() => onNavigate("settings")}><i className="nav-ic"><Settings size={16} /></i><span>设置</span></button>
        <button onClick={onToggleTheme} title="切换浅色/深色"><i className="nav-ic"><Moon size={16} /></i></button>
      </div>
    </aside>
  );
}

interface TrackActions {
  favoriteIds: Set<string>;
  onPlay: (track: Track) => void;
  onFavorite: (track: Track) => void;
  onDownload: (track: Track) => void;
}

function SearchPage({ query, tracks, loading, error, favoriteIds, currentTrackId, onPlay, onFavorite, onDownload, onLoadMore, loadingMore, canLoadMore, downloadedIds, inFlightIds }: TrackActions & {
  query: string;
  tracks: Track[];
  loading: boolean;
  error: string;
  currentTrackId: string;
  onLoadMore: () => void;
  loadingMore: boolean;
  canLoadMore: boolean;
  downloadedIds?: Set<string>;
  inFlightIds?: Set<string>;
}) {
  return (
    <section className="search-page">
      <div className="result-summary"><span>{query ? `找到 ${tracks.length} 个与 “${query}” 相关的结果` : "搜索所选音乐源"}</span><button>最匹配 <SlidersHorizontal size={14} /></button></div>
      <div className="track-table-header"><span>#</span><span>歌曲</span><span>歌手</span><span>专辑</span><span>来源</span><span>音质</span><span>时长</span><span /></div>
      <div className="track-list">
        {loading && <div className="center-state"><LoaderCircle className="spin" size={27} /><strong>正在搜索音乐</strong></div>}
        {!loading && error && <div className="center-state error"><AudioLines size={26} /><strong>音源暂时不可用</strong><small>{error.split("\n")[0]}</small></div>}
        {!loading && !error && !tracks.length && <div className="center-state"><Search size={26} /><strong>{query ? "没有找到可播放歌曲" : "输入歌曲、歌手或专辑"}</strong></div>}
        {!loading && !error && tracks.map((track, index) => (
          <TrackRow key={track.id} track={track} index={index} active={currentTrackId === track.id} favorite={favoriteIds.has(track.id)} onPlay={onPlay} onFavorite={onFavorite} onDownload={onDownload} downloadedIds={downloadedIds} inFlightIds={inFlightIds} />
        ))}
      </div>
      {!loading && !error && canLoadMore && tracks.length > 0 && <button className="load-more" onClick={onLoadMore}>{loadingMore ? "正在加载…" : "加载更多"}</button>}
    </section>
  );
}

function TrackListHeader() {
  return <div className="track-table-header"><span>#</span><span>歌曲</span><span>歌手</span><span>专辑</span><span>来源</span><span>音质</span><span>时长</span><span /></div>;
}

function TrackRow({ track, index, active, favorite, onPlay, onFavorite, onDownload, showRemoveFavorite = false, allowDownload = true, onRemoveFromLibrary, onRevealFile, downloadedIds, inFlightIds }: { track: Track; index: number; active: boolean; favorite: boolean; showRemoveFavorite?: boolean; allowDownload?: boolean; onRemoveFromLibrary?: (track: Track) => void; onRevealFile?: (track: Track) => void; downloadedIds?: Set<string>; inFlightIds?: Set<string> } & Omit<TrackActions, "favoriteIds">) {
  const source = getSourceMeta(track.source);
  const isDownloaded = downloadedIds?.has(track.id) ?? false;
  const isDownloading = inFlightIds?.has(track.id) ?? false;
  const revealPath = onRevealFile ? localPathOf(track) : undefined;
  return (
    <div className={`track-row ${active ? "playing" : ""}`}>
      <button className="row-play" title={active ? "正在播放" : "播放"} onClick={() => onPlay(track)}>{active ? <span className="eq" aria-hidden="true"><i /><i /><i /></span> : <><span>{String(index + 1).padStart(2, "0")}</span><Play size={14} fill="currentColor" /></>}</button>
      <button className="track-identity" onClick={() => onPlay(track)}><Artwork track={track} /><span><strong>{track.title}</strong></span></button>
      <span className="muted-cell">{track.artist}</span>
      <span className="muted-cell">{track.album}</span>
      <span className="source-cell"><i style={{ background: source.color }} /> <span>{source.shortLabel}</span></span>
      <span className="quality-cell"><QualityBadge track={track} /></span>
      <span className="duration-cell">{track.durationMs > 0 ? formatTime(track.durationMs / 1000) : "—"}</span>
      <span className="track-actions">
        {onRemoveFromLibrary && <button className="remove-favorite" onClick={() => onRemoveFromLibrary(track)} aria-label="从资料库移除" title="从资料库移除（不会删除本地文件）"><X size={16} /></button>}
        {revealPath && onRevealFile && <button className="reveal-file" onClick={() => onRevealFile(track)} aria-label="打开所在文件夹" title="打开所在文件夹"><FolderOpen size={16} /></button>}
        {showRemoveFavorite
          ? <button className="remove-favorite" onClick={() => onFavorite(track)} aria-label="移除收藏" title="移除收藏"><X size={16} /></button>
          : <button className={favorite ? "favorite" : ""} onClick={() => onFavorite(track)} aria-label={favorite ? "取消收藏" : "收藏"} title={favorite ? "取消收藏" : "收藏"}><Heart size={17} fill={favorite ? "currentColor" : "none"} /></button>}
        {allowDownload && (isDownloading
          ? <span className="download-state busy" title="下载中"><LoaderCircle className="spin" size={15} /></span>
          : isDownloaded
            ? <span className="download-state done" title="已下载"><Check size={15} /></span>
            : <button onClick={() => onDownload(track)} aria-label="下载" title="选择目录并下载"><Download size={17} /></button>)}
      </span>
    </div>
  );
}

function NowPlayingPanel({ track, error, queue, isPlaying, favorite, elapsed, duration, onTogglePlay, onFavorite, onPlay, onClearQueue, onOpenLyrics, pending = false, buffering = false }: {
  track: Track | null; queue: Track[]; isPlaying: boolean; favorite: boolean; elapsed: number; duration: number;
  error: string;
  onTogglePlay: () => void; onFavorite: () => void; onPlay: (track: Track) => void; onClearQueue: () => void; onOpenLyrics: () => void; pending?: boolean; buffering?: boolean;
}) {
  return (
    <aside className="now-panel">
      <div className="panel-title"><span><AudioLines size={16} /> 正在播放</span><button onClick={onOpenLyrics} aria-label="全屏歌词"><Maximize2 size={17} /></button></div>
      {track ? <>
        <div className="now-art"><Artwork track={track} /><div className="art-progress" style={{ width: `${duration ? elapsed / duration * 100 : 0}%` }} /></div>
        <div className="now-meta"><span><strong>{track.title}</strong><small>{track.artist} · {track.album}</small></span><QualityBadge track={track} /><button className={favorite ? "favorite" : ""} onClick={onFavorite}><Heart size={20} fill={favorite ? "currentColor" : "none"} /></button></div>
        {error && <p className="playback-error" role="alert">{error}</p>}
        <div className="panel-progress"><input type="range" min="0" max={duration || 1} value={elapsed} readOnly /><div><span>{formatTime(elapsed)}</span><span>{formatTime(duration)}</span></div></div>
        <div className="panel-controls"><button><Shuffle size={17} /></button><button><SkipBack size={19} fill="currentColor" /></button><button className="main-play" disabled={pending || buffering} onClick={onTogglePlay}>{(pending || buffering) ? <LoaderCircle className="spin" size={21} /> : isPlaying ? <Pause size={21} fill="currentColor" /> : <Play size={21} fill="currentColor" />}</button><button><SkipForward size={19} fill="currentColor" /></button><button><Repeat2 size={17} /></button></div>
        <div className="queue-heading"><span>播放队列 <small>{queue.length}</small></span><button onClick={onClearQueue}>清空</button></div>
        <div className="queue-list">{queue.map((item, index) => <button key={item.id} className={item.id === track.id ? "active" : ""} onClick={() => onPlay(item)}><span>{item.id === track.id ? <AudioLines size={13} /> : index + 1}</span><span><strong>{item.title}</strong><small>{item.artist}</small></span><em>{formatTime(item.durationMs / 1000)}</em><Ellipsis size={15} /></button>)}</div>
      </> : <div className="panel-empty"><span><Radio size={30} /></span><strong>等待第一首音乐</strong><small>搜索真实音源并选择一首歌曲</small></div>}
    </aside>
  );
}

function PlayerBar({ track, isPlaying, favorite, elapsed, duration, volume, onTogglePlay, onFavorite, onPrevious, onNext, onSeek, onVolume, onOpenLyrics, pending = false, buffering = false }: {
  track: Track | null; isPlaying: boolean; favorite: boolean; elapsed: number; duration: number; volume: number;
  onTogglePlay: () => void; onFavorite: () => void; onPrevious: () => void; onNext: () => void; onSeek: (value: number) => void; onVolume: (value: number) => void; onOpenLyrics: () => void; pending?: boolean; buffering?: boolean;
}) {
  return (
    <footer className="player-bar">
      <div className={`player-track ${track ? "" : "empty"}`}>{track ? <><Artwork track={track} /><span><strong>{track.title}</strong><small>{track.artist} · {track.album}</small></span><button className={favorite ? "favorite" : ""} onClick={onFavorite}><Heart size={18} fill={favorite ? "currentColor" : "none"} /></button><button onClick={onOpenLyrics} aria-label="全屏歌词"><MicVocal size={18} /></button></> : <><span className="empty-player-icon"><AudioLines size={18} /></span><span><strong>LiteMusicDL</strong><small>尚未播放</small></span></>}</div>
      <div className="transport"><div><button><Shuffle size={16} /></button><button onClick={onPrevious}><SkipBack size={19} fill="currentColor" /></button><button className="transport-play" disabled={pending || buffering} onClick={onTogglePlay}>{(pending || buffering) ? <LoaderCircle className="spin" size={20} /> : isPlaying ? <Pause size={20} fill="currentColor" /> : <Play size={20} fill="currentColor" />}</button><button onClick={onNext}><SkipForward size={19} fill="currentColor" /></button><button><Repeat2 size={16} /></button></div><span><small>{formatTime(elapsed)}</small><input type="range" min="0" max={duration || 1} value={elapsed} onChange={(event) => onSeek(Number(event.target.value))} style={{ "--progress": `${duration ? elapsed / duration * 100 : 0}%` } as React.CSSProperties} /><small>{formatTime(duration)}</small></span></div>
      <div className="player-volume"><Volume2 size={18} /><input type="range" min="0" max="1" step=".01" value={volume} onChange={(event) => onVolume(Number(event.target.value))} style={{ "--progress": `${volume * 100}%` } as React.CSSProperties} /><ListMusic size={19} /></div>
    </footer>
  );
}

function LibraryPage({ title, subtitle, tracks, empty, favoriteIds, onPlay, onFavorite, onDownload, downloadedIds, inFlightIds, onDownloadAll, onRevealFile }: TrackActions & { title: string; subtitle: string; tracks: Track[]; empty: string; downloadedIds?: Set<string>; inFlightIds?: Set<string>; onDownloadAll?: (tracks: Track[]) => void; onRevealFile?: (track: Track) => void }) {
  const isFavorites = title === "收藏";
  const [query, setQuery] = useState("");
  const q = query.trim().toLowerCase();
  const shownTracks = q
    ? tracks.filter((track) =>
        track.title.toLowerCase().includes(q)
        || track.artist.toLowerCase().includes(q)
        || track.album.toLowerCase().includes(q))
    : tracks;
  return <section className="library-page"><div className="library-title"><span><Library size={19} /></span><div><h1>{title}</h1><p>{subtitle}</p></div><div className="library-actions">{tracks.length > 0 && onDownloadAll && <button className="secondary" onClick={() => onDownloadAll(tracks)}><Download size={16} /> 下载全部{downloadedIds?.size ? `（${tracks.filter((t) => downloadedIds.has(t.id)).length} 已下载）` : ""}</button>}{tracks.length > 0 && <button onClick={() => onPlay(shownTracks[0])}><Play size={18} fill="currentColor" /> 播放全部</button>}</div></div>{tracks.length > 0 && <div className="local-search"><Search size={15} /><input value={query} onChange={(event) => setQuery(event.target.value)} placeholder={`搜索${title}（标题 / 歌手 / 专辑）`} aria-label={`搜索${title}`} />{query && <button className="local-search-clear" onClick={() => setQuery("")} aria-label="清空搜索"><X size={14} /></button>}</div>}{shownTracks.length ? <div className="library-list"><TrackListHeader />{shownTracks.map((track, index) => <TrackRow key={track.id} track={track} index={index} active={false} favorite={favoriteIds.has(track.id)} onPlay={onPlay} onFavorite={onFavorite} onDownload={onDownload} showRemoveFavorite={isFavorites} onRevealFile={onRevealFile} downloadedIds={downloadedIds} inFlightIds={inFlightIds} />)}</div> : <div className="center-state library-empty"><Album size={30} /><strong>{tracks.length ? (q ? "没有匹配的歌曲" : empty) : empty}</strong></div>}</section>;
}

function LocalMusicPage({ tracks, directory, scanning, error, hiddenCount, favoriteIds, currentTrackId, onChooseDirectory, onRescan, onRestoreHidden, onRemoveFromLibrary, onRevealFile, onPlay, onFavorite, onDownload, downloadedIds, inFlightIds, query, onQueryChange }: TrackActions & {
  tracks: Track[];
  directory: string;
  scanning: boolean;
  error: string;
  hiddenCount: number;
  currentTrackId: string;
  onChooseDirectory: () => void;
  onRescan: () => void;
  onRestoreHidden: () => void;
  onRemoveFromLibrary: (track: Track) => void;
  onRevealFile: (track: Track) => void;
  downloadedIds?: Set<string>;
  inFlightIds?: Set<string>;
  query: string;
  onQueryChange: (value: string) => void;
}) {
  const subtitle = scanning ? "正在扫描音乐文件…" : directory ? `${tracks.length} 首歌曲 · ${directory}` : "选择一个文件夹以建立本地音乐资料库";
  return <section className="library-page local-library-page">
    <div className="library-title">
      <span><FileMusic size={19} /></span>
      <div><h1>本地音乐</h1><p title={directory || undefined}>{subtitle}</p></div>
      <div className="library-actions">
        <button className="secondary" onClick={onChooseDirectory}><FolderOpen size={16} /> 选择文件夹</button>
        {hiddenCount > 0 && <button className="secondary" onClick={onRestoreHidden}>恢复已移除 {hiddenCount}</button>}
        {directory && <button onClick={onRescan} disabled={scanning}>{scanning ? <LoaderCircle className="spin" size={16} /> : <Search size={16} />} 重新扫描</button>}
      </div>
    </div>
    {error && <div className="local-scan-error" role="alert">{error}</div>}
    {directory && <div className="local-search"><Search size={15} /><input value={query} onChange={(event) => onQueryChange(event.target.value)} placeholder="搜索本地音乐（标题 / 歌手 / 专辑）" aria-label="搜索本地音乐" />{query && <button className="local-search-clear" onClick={() => onQueryChange("")} aria-label="清空搜索"><X size={14} /></button>}</div>}
    {scanning && !tracks.length ? <div className="center-state library-empty"><LoaderCircle className="spin" size={30} /><strong>正在扫描本地音乐</strong></div>
      : tracks.length ? <div className="library-list"><TrackListHeader />{tracks.map((track, index) => <TrackRow key={track.id} track={track} index={index} active={currentTrackId === track.id} favorite={favoriteIds.has(track.id)} onPlay={onPlay} onFavorite={onFavorite} onDownload={onDownload} allowDownload={false} onRemoveFromLibrary={onRemoveFromLibrary} onRevealFile={onRevealFile} downloadedIds={downloadedIds} inFlightIds={inFlightIds} />)}</div>
        : <div className="center-state library-empty"><FolderOpen size={30} /><strong>{directory ? (query ? "没有匹配的本地音乐" : "点击「重新扫描」开始扫描本地音乐") : "选择音乐文件夹开始扫描"}</strong></div>}
  </section>;
}

function DownloadsPage({ items, onStop, onRemove, onDeleteFile, onReveal, onPlay }: { items: DownloadItem[]; onStop: (id: string) => void; onRemove: (id: string) => void; onDeleteFile: (id: string) => void; onReveal: (item: DownloadItem) => void; onPlay: (track: Track) => void }) {
  const [query, setQuery] = useState("");
  const q = query.trim().toLowerCase();
  const shownItems = q ? items.filter((item) =>
    item.track.title.toLowerCase().includes(q)
    || item.track.artist.toLowerCase().includes(q)
    || item.track.album.toLowerCase().includes(q)
    || (item.path || "").toLowerCase().includes(q)) : items;
  return <section className="downloads-page"><div className="library-title"><span><Download size={19} /></span><div><h1>下载管理</h1><p>{items.filter((item) => item.status === "completed").length} 个任务已完成 · 有歌词时自动保存同名 LRC</p></div></div>{items.length > 0 && <div className="local-search"><Search size={15} /><input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="搜索下载（标题 / 歌手 / 专辑 / 路径）" aria-label="搜索下载" />{query && <button className="local-search-clear" onClick={() => setQuery("")} aria-label="清空搜索"><X size={14} /></button>}</div>}{!items.length ? <div className="center-state library-empty"><Download size={30} /><strong>还没有下载任务</strong><small>在歌曲右侧点击下载按钮并选择保存目录</small></div> : shownItems.length ? <div className="download-list">{shownItems.map((item) => <div className="download-row" key={item.id}><Artwork track={item.track} /><span><strong>{item.track.title}</strong><small>{item.track.artist} · {item.track.format}</small></span><div><i><em style={{ width: `${item.progress}%` }} /></i><small>{item.status === "completed" ? item.path : item.status === "failed" ? item.error : "正在下载音频与可用歌词"}</small></div><span className={`download-status ${item.status}`}>{item.status === "completed" ? <Check size={15} /> : item.status === "failed" ? <X size={15} /> : `${item.progress}%`}</span><span className="download-actions">{item.status === "completed" && <button onClick={() => onPlay(item.track)} title="播放" aria-label="播放"><Play size={14} fill="currentColor" /></button>}{item.status === "downloading" && <button onClick={() => onStop(item.id)} title="停止下载" aria-label="停止下载"><Pause size={14} fill="currentColor" /></button>}{item.path && <button onClick={() => onReveal(item)} title="打开所在文件夹" aria-label="打开所在文件夹"><FolderOpen size={14} /></button>}{item.path && <button onClick={() => onDeleteFile(item.id)} title="删除本地文件（含 LRC）" aria-label="删除本地文件"><Trash2 size={15} /></button>}<button onClick={() => onRemove(item.id)} title="删除记录" aria-label="删除记录"><X size={15} /></button></span></div>)}</div> : <div className="center-state library-empty"><Download size={30} /><strong>没有匹配的下载</strong></div>}</section>;
}

function SettingsPage({ defaultDownloadPath, onChangeDownloadPath, gradient, onToggleGradient }: {
  defaultDownloadPath: string;
  onChangeDownloadPath: (path: string) => void;
  gradient: boolean;
  onToggleGradient: () => void;
}) {
  const [message, setMessage] = useState("");

  async function selectDirectory() {
    setMessage("");
    try {
      const path = await chooseDownloadDirectory(defaultDownloadPath);
      if (path) {
        onChangeDownloadPath(path);
        setMessage("默认下载目录已更新");
      }
    } catch (directoryError) {
      setMessage(`目录选择失败: ${String(directoryError)}`);
    }
  }

  return (
    <section className="settings-page">
      <div className="library-title"><span><Settings size={19} /></span><div><h1>设置</h1></div></div>
      <div className="settings-card">
        <div className="settings-icon"><FolderOpen size={21} /></div>
        <div className="settings-copy"><strong>默认下载保存路径</strong><small>点击歌曲下载时会以该目录为起点，再由你确认实际保存位置。</small></div>
        <div className="path-field" title={defaultDownloadPath || "尚未设置"}>{defaultDownloadPath || "尚未设置，将使用系统默认目录"}</div>
        <button onClick={selectDirectory}><FolderOpen size={16} /> 选择目录</button>
      </div>
      <div className="settings-card"><div className="settings-icon"><Moon size={21} /></div><div className="settings-copy"><strong>渐变背景</strong><small>为工作区启用柔和的渐变色背景。</small></div><button onClick={onToggleGradient}>{gradient ? "已开启" : "开启渐变"}</button></div>
      {message && <p className="settings-message">{message}</p>}
    </section>
  );
}

interface LyricLine {
  time: number;
  text: string;
}

function parseLyrics(value: string): LyricLine[] {
  const timed: LyricLine[] = [];
  for (const rawLine of value.split(/\r?\n/)) {
    const text = rawLine.replace(/\[(\d{1,3}):(\d{2}(?:\.\d{1,3})?)\]/g, "").trim();
    const timestamps = [...rawLine.matchAll(/\[(\d{1,3}):(\d{2}(?:\.\d{1,3})?)\]/g)];
    for (const timestamp of timestamps) {
      timed.push({ time: Number(timestamp[1]) * 60 + Number(timestamp[2]), text: text || "…" });
    }
  }
  if (timed.length) return timed.sort((a, b) => a.time - b.time);
  return value.split(/\r?\n/).map((text) => text.trim()).filter(Boolean).map((text) => ({ time: -1, text }));
}

function LyricsOverlay({ track, lyrics, loading, error, elapsed, duration, isPlaying, onClose, onTogglePlay, onPrevious, onNext, onSeek, pending = false, buffering = false }: {
  track: Track;
  lyrics: string;
  loading: boolean;
  error: string;
  elapsed: number;
  duration: number;
  isPlaying: boolean;
  onClose: () => void;
  onTogglePlay: () => void;
  onPrevious: () => void;
  onNext: () => void;
  onSeek: (value: number) => void;
  pending?: boolean;
  buffering?: boolean;
}) {
  const lines = useMemo(() => parseLyrics(lyrics), [lyrics]);
  const activeIndex = lines.reduce((active, line, index) => line.time >= 0 && line.time <= elapsed ? index : active, -1);
  const activeRef = useRef<HTMLParagraphElement>(null);
  const source = getSourceMeta(track.source);

  useEffect(() => {
    activeRef.current?.scrollIntoView({ block: "center", behavior: "smooth" });
  }, [activeIndex]);
  useEffect(() => {
    const closeOnEscape = (event: KeyboardEvent) => event.key === "Escape" && onClose();
    window.addEventListener("keydown", closeOnEscape);
    return () => window.removeEventListener("keydown", closeOnEscape);
  }, [onClose]);

  return (
    <section className="fullplayer" aria-label="全屏播放与歌词">
      {track.artworkUrl && <div className="fullplayer-bg" style={{ backgroundImage: `url(${track.artworkUrl})` }} />}
      <div className="fullplayer-veil" />
      <header className="fullplayer-header">
        <span><AudioLines size={17} /> LiteMusicDL</span>
        <div className="fullplayer-header-meta"><span>正在播放</span><QualityBadge track={track} /></div>
        <button onClick={onClose}><X size={18} /> <em>退出全屏</em></button>
      </header>
      <div className="fullplayer-stage">
        <div className="fullplayer-main glass">
          <div className="fullplayer-art">
            <Artwork track={track} />
            <span className="fullplayer-src" style={{ background: source.color }}>{source.shortLabel}</span>
          </div>
          <div className="fullplayer-info">
            <div className="fullplayer-meta">
              <span><h1>{track.title}</h1><h2>{track.artist}</h2><small>{track.album}</small></span>
              <QualityBadge track={track} />
            </div>
            <div className="fullplayer-progress"><span>{formatTime(elapsed)}</span><input type="range" min="0" max={duration || 1} value={elapsed} onChange={(event) => onSeek(Number(event.target.value))} style={{ "--progress": `${duration ? elapsed / duration * 100 : 0}%` } as React.CSSProperties} /><span>{formatTime(duration)}</span></div>
            <div className="fullplayer-transport">
              <button><Shuffle size={18} /></button>
              <button onClick={onPrevious}><SkipBack size={20} fill="currentColor" /></button>
              <button className="main-play" disabled={pending || buffering} onClick={onTogglePlay}>{(pending || buffering) ? <LoaderCircle className="spin" size={24} /> : isPlaying ? <Pause size={24} fill="currentColor" /> : <Play size={24} fill="currentColor" />}</button>
              <button onClick={onNext}><SkipForward size={20} fill="currentColor" /></button>
              <button><Repeat2 size={18} /></button>
            </div>
          </div>
        </div>
        <div className="fullplayer-lyrics glass">
          <div className="lyrics-heading"><MicVocal size={18} /><span>歌词</span></div>
          <div className="lyrics-scroll">
            {loading && <div className="lyrics-state"><LoaderCircle className="spin" size={24} /><span>正在从音源获取歌词</span></div>}
            {!loading && error && <div className="lyrics-state error"><MicVocal size={24} /><span>歌词加载失败</span><small>{error.split("\n")[0]}</small></div>}
            {!loading && !error && !lines.length && <div className="lyrics-state"><MicVocal size={24} /><span>此歌曲暂无可用歌词</span></div>}
            {!loading && !error && lines.map((line, index) => <p ref={index === activeIndex ? activeRef : undefined} className={index === activeIndex ? "active" : ""} key={`${line.time}-${index}`}>{line.text}</p>)}
          </div>
        </div>
      </div>
    </section>
  );
}

function Artwork({ track }: { track: Track }) {
  return track.artworkUrl
    ? <img className="artwork" src={track.artworkUrl} alt={track.album || track.title} />
    : <span className="artwork artwork-empty"><Album size={20} /></span>;
}

/** Small quality pill shown in the dedicated 音质 column. */
function QualityBadge({ track }: { track: Track }) {
  const label = qualityOf(track);
  if (!label) return <span className="quality-badge unknown">—</span>;
  return <span className={`quality-badge ${qualityTone(label)}`}>{label}</span>;
}

function formatTime(seconds: number) {
  if (!Number.isFinite(seconds) || seconds < 0) return "0:00";
  return `${Math.floor(seconds / 60)}:${Math.floor(seconds % 60).toString().padStart(2, "0")}`;
}
