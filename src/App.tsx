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
  Volume2,
  X
} from "lucide-react";
import { FormEvent, useEffect, useMemo, useRef, useState } from "react";
import { allSources, defaultSources, getSourceMeta, localSource } from "./data/sources";
import { chooseDownloadDirectory, chooseMusicDirectory, downloadTrack, fetchLyrics, loadDownloadHistory, preparePlayback, saveDownloadHistory, scanLocalMusic, searchTracks } from "./lib/bridge";
import type { DownloadItem, Track } from "./types";

type Page = "search" | "discover" | "favorites" | "downloads" | "local" | "history" | "settings";

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

export default function App() {
  const [lightTheme, setLightTheme] = useState(() => localStorage.getItem("litemusic:theme") === "light");
  const [gradient, setGradient] = useState(() => localStorage.getItem("litemusic:gradient") === "on");
  const [page, setPage] = useState<Page>("search");
  const [query, setQuery] = useState("");
  const [searchedQuery, setSearchedQuery] = useState("");
  const [tracks, setTracks] = useState<Track[]>([]);
  const [loading, setLoading] = useState(false);
  const [loadingMore, setLoadingMore] = useState(false);
  const [resultLimit, setResultLimit] = useState(20);
  const [error, setError] = useState("");
  const [playbackError, setPlaybackError] = useState("");
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
  const [volume, setVolume] = useState(.76);
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
  const scannedDirectoryRef = useRef("");
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
  useEffect(() => {
    if (page !== "local" || !localDirectory || scannedDirectoryRef.current === localDirectory) return;
    scanLocalLibrary(localDirectory);
  }, [page, localDirectory]);

  const favoriteIds = useMemo(() => new Set(favorites.map((track) => track.id)), [favorites]);
  const visibleTracks = useMemo(() => tracks.filter((track) => selectedSources.includes(track.source)), [selectedSources, tracks]);
  const visibleLocalTracks = useMemo(() => localTracks.filter((track) => !hiddenLocalTrackIds.includes(track.id)), [hiddenLocalTrackIds, localTracks]);
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
    setResultLimit(20);
    setError("");
    try {
      const result = await searchTracks(query, selectedSources, 20);
      if (token === searchTokenRef.current) setTracks(result);
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
    const nextLimit = resultLimit + 20;
    try {
      const next = await searchTracks(searchedQuery, selectedSources, nextLimit);
      setTracks((current) => [...current, ...next.filter((track) => !current.some((item) => item.id === track.id))]);
      setResultLimit(nextLimit);
    } catch (loadError) { setError(String(loadError)); }
    finally { setLoadingMore(false); }
  }

  async function scanLocalLibrary(directory = localDirectory) {
    const selectedDirectory = directory.trim();
    if (!selectedDirectory) return;
    const token = ++localScanTokenRef.current;
    scannedDirectoryRef.current = selectedDirectory;
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
    let playable = track;
    setPlaybackError("");
    try {
      playable = { ...track, audioUrl: await preparePlayback(track) };
    } catch (playError) {
      setPlaybackError(`无法准备播放：${String(playError)}`);
      return;
    }
    setCurrentTrack(playable);
    setElapsed(0);
    setDuration(track.durationMs / 1000 || 0);
    setIsPlaying(true);
    setQueue((items) => items.some((item) => item.id === track.id) ? items : [track, ...items]);
    setHistory((items) => [track, ...items.filter((item) => item.id !== track.id)].slice(0, 50));
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
    try {
      const path = await downloadTrack(track, directory);
      setDownloads((items) => items.map((item) => item.id === id
        ? { ...item, status: "completed", progress: 100, path }
        : item));
    } catch (downloadError) {
      setDownloads((items) => items.map((item) => item.id === id
        ? { ...item, status: "failed", progress: 0, error: String(downloadError) }
        : item));
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
            canLoadMore
          />
        )}
        {page === "favorites" && (
          <LibraryPage title="收藏" subtitle={`${favorites.length} 首歌曲`} tracks={favorites} empty="把想再听的音乐留在这里。" favoriteIds={favoriteIds} onPlay={play} onFavorite={toggleFavorite} onDownload={startDownload} />
        )}
        {page === "downloads" && <DownloadsPage items={downloads} />}
        {page === "discover" && (
          <LibraryPage title="发现" subtitle="仅展示音源实时返回的音乐" tracks={tracks} empty="搜索后，真实结果会出现在这里。" favoriteIds={favoriteIds} onPlay={play} onFavorite={toggleFavorite} onDownload={startDownload} />
        )}
        {page === "local" && <LocalMusicPage tracks={visibleLocalTracks} directory={localDirectory} scanning={localScanning} error={localError} hiddenCount={hiddenLocalTrackIds.length} favoriteIds={favoriteIds} currentTrackId={currentTrack?.id ?? ""} onChooseDirectory={selectLocalDirectory} onRescan={() => scanLocalLibrary()} onRestoreHidden={() => setHiddenLocalTrackIds([])} onRemoveFromLibrary={hideLocalTrack} onPlay={play} onFavorite={toggleFavorite} onDownload={startDownload} />}
        {page === "history" && <LibraryPage title="最近播放" subtitle="继续刚才的声音" tracks={history} empty="还没有播放记录。" favoriteIds={favoriteIds} onPlay={play} onFavorite={toggleFavorite} onDownload={startDownload} />}
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
        onTogglePlay={() => setIsPlaying((value) => !value)}
        onFavorite={() => currentTrack && toggleFavorite(currentTrack)}
        onPlay={play}
        onClearQueue={() => setQueue(currentTrack ? [currentTrack] : [])}
        onOpenLyrics={() => currentTrack && setShowLyrics(true)}
      />

      <PlayerBar
        track={currentTrack}
        isPlaying={isPlaying}
        favorite={currentTrack ? favoriteIds.has(currentTrack.id) : false}
        elapsed={elapsed}
        duration={duration}
        volume={volume}
        onTogglePlay={() => setIsPlaying((value) => !value)}
        onFavorite={() => currentTrack && toggleFavorite(currentTrack)}
        onPrevious={() => playRelative(-1)}
        onNext={() => playRelative(1)}
        onSeek={(value) => {
          setElapsed(value);
          if (audioRef.current) audioRef.current.currentTime = value;
        }}
        onVolume={setVolume}
        onOpenLyrics={() => currentTrack && setShowLyrics(true)}
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
          onClose={() => setShowLyrics(false)}
          onTogglePlay={() => setIsPlaying((value) => !value)}
          onPrevious={() => playRelative(-1)}
          onNext={() => playRelative(1)}
          onSeek={(value) => {
            setElapsed(value);
            if (audioRef.current) audioRef.current.currentTime = value;
          }}
        />
      )}

      <audio
        ref={audioRef}
        src={currentTrack?.audioUrl}
        onLoadedMetadata={(event) => setDuration(event.currentTarget.duration)}
        onTimeUpdate={(event) => setElapsed(event.currentTarget.currentTime)}
        onError={(event) => {
          const code = event.currentTarget.error?.code;
          setPlaybackError(`播放请求失败${code ? `（音频错误 ${code}）` : ""}`);
          setIsPlaying(false);
        }}
        onEnded={() => playRelative(1)}
      />
    </div>
  );
}

function SourceStrip({ selectedSources, onToggle }: { selectedSources: string[]; onToggle: (source: string) => void }) {
  return <div className="source-strip"><div className="source-selection"><span className="source-strip-label"><Filter size={14} /> 搜索来源</span><div className="source-strip-options">{allSources.map((source) => { const meta = getSourceMeta(source); const selected = selectedSources.includes(source); return <button key={source} className={selected ? "selected" : ""} onClick={() => onToggle(source)} aria-pressed={selected}><i style={{ background: meta.color }} /><span>{meta.label}</span><b>{selected ? "✓" : ""}</b></button>; })}</div></div></div>;
}

function Sidebar({ page, onNavigate, onToggleTheme }: { page: Page; onNavigate: (page: Page) => void; onToggleTheme: () => void }) {
  return (
    <aside className="sidebar">
      <nav>
        {navGroups.map((group) => (
          <div className="nav-group" key={group.label}>
            <p>{group.label}</p>
            {group.items.map((item) => {
              const Icon = item.icon;
              return <button key={item.id} className={page === item.id ? "active" : ""} onClick={() => onNavigate(item.id)}><Icon size={18} /><span>{item.label}</span></button>;
            })}
          </div>
        ))}
      </nav>
      <div className="sidebar-bottom">
        <button className={page === "settings" ? "active" : ""} onClick={() => onNavigate("settings")}><Settings size={18} /><span>设置</span></button>
        <button onClick={onToggleTheme} title="切换浅色/深色"><Moon size={18} /></button>
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

function SearchPage({ query, tracks, loading, error, favoriteIds, currentTrackId, onPlay, onFavorite, onDownload, onLoadMore, loadingMore, canLoadMore }: TrackActions & {
  query: string;
  tracks: Track[];
  loading: boolean;
  error: string;
  currentTrackId: string;
  onLoadMore: () => void;
  loadingMore: boolean;
  canLoadMore: boolean;
}) {
  return (
    <section className="search-page">
      <div className="result-summary"><span>{query ? `找到 ${tracks.length} 个与 “${query}” 相关的结果` : "搜索所选音乐源"}</span><button>最匹配 <SlidersHorizontal size={14} /></button></div>
      <div className="track-table-header"><span>#</span><span>歌曲</span><span>歌手</span><span>专辑</span><span>来源 / 音质</span><span>时长</span><span /></div>
      <div className="track-list">
        {loading && <div className="center-state"><LoaderCircle className="spin" size={27} /><strong>正在搜索音乐</strong></div>}
        {!loading && error && <div className="center-state error"><AudioLines size={26} /><strong>音源暂时不可用</strong><small>{error.split("\n")[0]}</small></div>}
        {!loading && !error && !tracks.length && <div className="center-state"><Search size={26} /><strong>{query ? "没有找到可播放歌曲" : "输入歌曲、歌手或专辑"}</strong></div>}
        {!loading && !error && tracks.map((track, index) => (
          <TrackRow key={track.id} track={track} index={index} active={currentTrackId === track.id} favorite={favoriteIds.has(track.id)} onPlay={onPlay} onFavorite={onFavorite} onDownload={onDownload} />
        ))}
      </div>
      {!loading && !error && canLoadMore && tracks.length > 0 && <button className="load-more" onClick={onLoadMore}>{loadingMore ? "正在加载…" : "加载更多"}</button>}
    </section>
  );
}

function TrackRow({ track, index, active, favorite, onPlay, onFavorite, onDownload, showRemoveFavorite = false, allowDownload = true, onRemoveFromLibrary }: { track: Track; index: number; active: boolean; favorite: boolean; showRemoveFavorite?: boolean; allowDownload?: boolean; onRemoveFromLibrary?: (track: Track) => void } & Omit<TrackActions, "favoriteIds">) {
  const source = getSourceMeta(track.source);
  return (
    <div className={`track-row ${active ? "playing" : ""}`}>
      <button className="row-play" title="播放" onClick={() => onPlay(track)}>{active ? <AudioLines size={16} /> : <><span>{String(index + 1).padStart(2, "0")}</span><Play size={14} fill="currentColor" /></>}</button>
      <button className="track-identity" onClick={() => onPlay(track)}><Artwork track={track} /><span><strong>{track.title}</strong></span></button>
      <span className="muted-cell">{track.artist}</span>
      <span className="muted-cell">{track.album}</span>
      <span className="source-cell"><i style={{ background: source.color }} /> <span>{source.shortLabel}{track.quality && <small>{track.quality}</small>}</span></span>
      <span className="duration-cell">{track.durationMs > 0 ? formatTime(track.durationMs / 1000) : "—"}</span>
      <span className="track-actions">
        {onRemoveFromLibrary && <button className="remove-favorite" onClick={() => onRemoveFromLibrary(track)} aria-label="从资料库移除" title="从资料库移除（不会删除本地文件）"><X size={16} /></button>}
        {showRemoveFavorite
          ? <button className="remove-favorite" onClick={() => onFavorite(track)} aria-label="移除收藏" title="移除收藏"><X size={16} /></button>
          : <button className={favorite ? "favorite" : ""} onClick={() => onFavorite(track)} aria-label={favorite ? "取消收藏" : "收藏"} title={favorite ? "取消收藏" : "收藏"}><Heart size={17} fill={favorite ? "currentColor" : "none"} /></button>}
        {allowDownload && <button onClick={() => onDownload(track)} aria-label="下载" title="选择目录并下载"><Download size={17} /></button>}
      </span>
    </div>
  );
}

function NowPlayingPanel({ track, error, queue, isPlaying, favorite, elapsed, duration, onTogglePlay, onFavorite, onPlay, onClearQueue, onOpenLyrics }: {
  track: Track | null; queue: Track[]; isPlaying: boolean; favorite: boolean; elapsed: number; duration: number;
  error: string;
  onTogglePlay: () => void; onFavorite: () => void; onPlay: (track: Track) => void; onClearQueue: () => void; onOpenLyrics: () => void;
}) {
  return (
    <aside className="now-panel">
      <div className="panel-title"><span><AudioLines size={16} /> 正在播放</span><button onClick={onOpenLyrics} aria-label="全屏歌词"><Maximize2 size={17} /></button></div>
      {track ? <>
        <div className="now-art"><Artwork track={track} /><div className="art-progress" style={{ width: `${duration ? elapsed / duration * 100 : 0}%` }} /></div>
        <div className="now-meta"><span><strong>{track.title}</strong><small>{track.artist} · {track.album}</small></span><button className={favorite ? "favorite" : ""} onClick={onFavorite}><Heart size={20} fill={favorite ? "currentColor" : "none"} /></button></div>
        {error && <p className="playback-error" role="alert">{error}</p>}
        <div className="panel-progress"><input type="range" min="0" max={duration || 1} value={elapsed} readOnly /><div><span>{formatTime(elapsed)}</span><span>{formatTime(duration)}</span></div></div>
        <div className="panel-controls"><button><Shuffle size={17} /></button><button><SkipBack size={19} fill="currentColor" /></button><button className="main-play" onClick={onTogglePlay}>{isPlaying ? <Pause size={21} fill="currentColor" /> : <Play size={21} fill="currentColor" />}</button><button><SkipForward size={19} fill="currentColor" /></button><button><Repeat2 size={17} /></button></div>
        <div className="queue-heading"><span>播放队列 <small>{queue.length}</small></span><button onClick={onClearQueue}>清空</button></div>
        <div className="queue-list">{queue.map((item, index) => <button key={item.id} className={item.id === track.id ? "active" : ""} onClick={() => onPlay(item)}><span>{item.id === track.id ? <AudioLines size={13} /> : index + 1}</span><span><strong>{item.title}</strong><small>{item.artist}</small></span><em>{formatTime(item.durationMs / 1000)}</em><Ellipsis size={15} /></button>)}</div>
      </> : <div className="panel-empty"><span><Radio size={30} /></span><strong>等待第一首音乐</strong><small>搜索真实音源并选择一首歌曲</small></div>}
    </aside>
  );
}

function PlayerBar({ track, isPlaying, favorite, elapsed, duration, volume, onTogglePlay, onFavorite, onPrevious, onNext, onSeek, onVolume, onOpenLyrics }: {
  track: Track | null; isPlaying: boolean; favorite: boolean; elapsed: number; duration: number; volume: number;
  onTogglePlay: () => void; onFavorite: () => void; onPrevious: () => void; onNext: () => void; onSeek: (value: number) => void; onVolume: (value: number) => void; onOpenLyrics: () => void;
}) {
  return (
    <footer className="player-bar">
      <div className={`player-track ${track ? "" : "empty"}`}>{track ? <><Artwork track={track} /><span><strong>{track.title}</strong><small>{track.artist} · {track.album}</small></span><button className={favorite ? "favorite" : ""} onClick={onFavorite}><Heart size={18} fill={favorite ? "currentColor" : "none"} /></button><button onClick={onOpenLyrics} aria-label="全屏歌词"><MicVocal size={18} /></button></> : <><span className="empty-player-icon"><AudioLines size={18} /></span><span><strong>LiteMusicDL</strong><small>尚未播放</small></span></>}</div>
      <div className="transport"><div><button><Shuffle size={16} /></button><button onClick={onPrevious}><SkipBack size={19} fill="currentColor" /></button><button className="transport-play" onClick={onTogglePlay}>{isPlaying ? <Pause size={20} fill="currentColor" /> : <Play size={20} fill="currentColor" />}</button><button onClick={onNext}><SkipForward size={19} fill="currentColor" /></button><button><Repeat2 size={16} /></button></div><span><small>{formatTime(elapsed)}</small><input type="range" min="0" max={duration || 1} value={elapsed} onChange={(event) => onSeek(Number(event.target.value))} style={{ "--progress": `${duration ? elapsed / duration * 100 : 0}%` } as React.CSSProperties} /><small>{formatTime(duration)}</small></span></div>
      <div className="player-volume"><Volume2 size={18} /><input type="range" min="0" max="1" step=".01" value={volume} onChange={(event) => onVolume(Number(event.target.value))} style={{ "--progress": `${volume * 100}%` } as React.CSSProperties} /><ListMusic size={19} /></div>
    </footer>
  );
}

function LibraryPage({ title, subtitle, tracks, empty, favoriteIds, onPlay, onFavorite, onDownload }: TrackActions & { title: string; subtitle: string; tracks: Track[]; empty: string }) {
  const isFavorites = title === "收藏";
  return <section className="library-page"><div className="library-title"><span><Library size={19} /></span><div><h1>{title}</h1><p>{subtitle}</p></div>{tracks.length > 0 && <button onClick={() => onPlay(tracks[0])}><Play size={18} fill="currentColor" /> 播放全部</button>}</div>{tracks.length ? <div className="library-list">{tracks.map((track, index) => <TrackRow key={track.id} track={track} index={index} active={false} favorite={favoriteIds.has(track.id)} onPlay={onPlay} onFavorite={onFavorite} onDownload={onDownload} showRemoveFavorite={isFavorites} />)}</div> : <div className="center-state library-empty"><Album size={30} /><strong>{empty}</strong></div>}</section>;
}

function LocalMusicPage({ tracks, directory, scanning, error, hiddenCount, favoriteIds, currentTrackId, onChooseDirectory, onRescan, onRestoreHidden, onRemoveFromLibrary, onPlay, onFavorite, onDownload }: TrackActions & {
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
    {scanning && !tracks.length ? <div className="center-state library-empty"><LoaderCircle className="spin" size={30} /><strong>正在扫描本地音乐</strong></div>
      : tracks.length ? <div className="library-list">{tracks.map((track, index) => <TrackRow key={track.id} track={track} index={index} active={currentTrackId === track.id} favorite={favoriteIds.has(track.id)} onPlay={onPlay} onFavorite={onFavorite} onDownload={onDownload} allowDownload={false} onRemoveFromLibrary={onRemoveFromLibrary} />)}</div>
        : <div className="center-state library-empty"><FolderOpen size={30} /><strong>{directory ? "没有找到支持的音频文件" : "选择音乐文件夹开始扫描"}</strong></div>}
  </section>;
}

function DownloadsPage({ items }: { items: DownloadItem[] }) {
  return <section className="downloads-page"><div className="library-title"><span><Download size={19} /></span><div><h1>下载管理</h1><p>{items.filter((item) => item.status === "completed").length} 个任务已完成 · 有歌词时自动保存同名 LRC</p></div></div>{!items.length ? <div className="center-state library-empty"><Download size={30} /><strong>还没有下载任务</strong><small>在歌曲右侧点击下载按钮并选择保存目录</small></div> : <div className="download-list">{items.map((item) => <div className="download-row" key={item.id}><Artwork track={item.track} /><span><strong>{item.track.title}</strong><small>{item.track.artist} · {item.track.format}</small></span><div><i><em style={{ width: `${item.progress}%` }} /></i><small>{item.status === "completed" ? item.path : item.status === "failed" ? item.error : "正在下载音频与可用歌词"}</small></div><span className={`download-status ${item.status}`}>{item.status === "completed" ? <Check size={15} /> : item.status === "failed" ? <X size={15} /> : `${item.progress}%`}</span></div>)}</div>}</section>;
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

function LyricsOverlay({ track, lyrics, loading, error, elapsed, duration, isPlaying, onClose, onTogglePlay, onPrevious, onNext, onSeek }: {
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
}) {
  const lines = useMemo(() => parseLyrics(lyrics), [lyrics]);
  const activeIndex = lines.reduce((active, line, index) => line.time >= 0 && line.time <= elapsed ? index : active, -1);
  const activeRef = useRef<HTMLParagraphElement>(null);

  useEffect(() => {
    activeRef.current?.scrollIntoView({ block: "center", behavior: "smooth" });
  }, [activeIndex]);
  useEffect(() => {
    const closeOnEscape = (event: KeyboardEvent) => event.key === "Escape" && onClose();
    window.addEventListener("keydown", closeOnEscape);
    return () => window.removeEventListener("keydown", closeOnEscape);
  }, [onClose]);

  return (
    <section className="lyrics-overlay" aria-label="全屏播放与歌词">
      <header><span><AudioLines size={17} /> LiteMusicDL · 正在播放</span><button onClick={onClose}><X size={20} /> 退出全屏</button></header>
      <div className="lyrics-stage">
        <div className="lyrics-album">
          <Artwork track={track} />
          <div><h1>{track.title}</h1><h2>{track.artist}</h2><small>{track.album}</small></div>
          <div className="lyrics-transport">
            <button onClick={onPrevious}><SkipBack size={21} fill="currentColor" /></button>
            <button className="lyrics-play" onClick={onTogglePlay}>{isPlaying ? <Pause size={25} fill="currentColor" /> : <Play size={25} fill="currentColor" />}</button>
            <button onClick={onNext}><SkipForward size={21} fill="currentColor" /></button>
          </div>
          <div className="lyrics-progress"><span>{formatTime(elapsed)}</span><input type="range" min="0" max={duration || 1} value={elapsed} onChange={(event) => onSeek(Number(event.target.value))} /><span>{formatTime(duration)}</span></div>
        </div>
        <div className="lyrics-pane">
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

function formatTime(seconds: number) {
  if (!Number.isFinite(seconds) || seconds < 0) return "0:00";
  return `${Math.floor(seconds / 60)}:${Math.floor(seconds % 60).toString().padStart(2, "0")}`;
}
