export const sourceMeta = {
  QQMusicClient: {
    label: "QQ音乐",
    shortLabel: "QQ音乐",
    color: "#31c27c",
    enabled: true
  },
  KuwoMusicClient: {
    label: "酷我音乐",
    shortLabel: "酷我",
    color: "#ffb51b",
    enabled: true
  },
  NeteaseMusicClient: {
    label: "网易云音乐",
    shortLabel: "网易云",
    color: "#ef3e49",
    enabled: true
  },
  MiguMusicClient: {
    label: "咪咕音乐",
    shortLabel: "咪咕",
    color: "#1f8fff",
    enabled: true
  }
} as const;

export const allSources = Object.keys(sourceMeta);
export const defaultSources = ["QQMusicClient", "KuwoMusicClient"];
export const localSource = "LocalMusicClient";

const localSourceMeta = {
  label: "本地音乐",
  shortLabel: "本地",
  color: "#8fa66e",
  enabled: true
};

export function getSourceMeta(source: string) {
  if (source === localSource) return localSourceMeta;
  return sourceMeta[source as keyof typeof sourceMeta] ?? {
    label: source.replace(/MusicClient$/, ""),
    shortLabel: source.replace(/MusicClient$/, ""),
    color: "#9d9b95",
    enabled: false
  };
}
