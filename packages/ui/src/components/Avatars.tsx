import type { SyncPeer } from "../api";

/** 名字 → 稳定的色相，同一个人在所有界面颜色一致。 */
const hue = (name: string) => {
  let h = 0;
  for (const ch of name) h = (h * 31 + ch.codePointAt(0)!) % 360;
  return h;
};

export function Avatar({ name, size = 22 }: { name: string; size?: number }) {
  const initial = [...name.trim()][0] ?? "?";
  return (
    <span
      className="avatar"
      title={name}
      style={{
        width: size,
        height: size,
        fontSize: Math.round(size * 0.5),
        background: `hsl(${hue(name)} 42% 40%)`,
      }}
    >
      {initial}
    </span>
  );
}

/** 在线成员头像堆叠；同一个人在多台电脑上只显示一次。 */
export function Avatars({ peers, max = 4 }: { peers: SyncPeer[]; max?: number }) {
  const names = [...new Set(peers.map((p) => p.user))];
  if (names.length === 0) return null;
  const shown = names.slice(0, max);
  return (
    <span className="avatars" aria-label={`在线成员：${names.join("、")}`}>
      {shown.map((n) => (
        <Avatar key={n} name={n} />
      ))}
      {names.length > shown.length && <span className="avatar more">+{names.length - shown.length}</span>}
    </span>
  );
}
