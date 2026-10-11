/** 新事项按标题稳定分配柔和颜色；编辑和改期沿用已保存的颜色。 */
export function automaticColor(title: string): string {
  const palette = ["sage", "blue", "amber", "violet", "rose", "gray"];
  let hash = 2166136261;
  for (const char of title.trim().normalize("NFC")) {
    hash = Math.imul(hash ^ char.codePointAt(0)!, 16777619);
  }
  return palette[(hash >>> 0) % palette.length];
}
