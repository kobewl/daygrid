const formatters = new Map<string, Intl.DateTimeFormat>();

/** 当前日历时区的纯日期；不使用电脑时区来猜服务端的「今天」。 */
export function dateInTimeZone(timeZone: string, now = Date.now()): string {
  let formatter = formatters.get(timeZone);
  if (!formatter) {
    formatter = new Intl.DateTimeFormat("en-US", {
      timeZone,
      year: "numeric",
      month: "2-digit",
      day: "2-digit",
    });
    formatters.set(timeZone, formatter);
  }
  const parts = new Map(
    formatter.formatToParts(now).map((p) => [p.type, p.value]),
  );
  return `${parts.get("year")}-${parts.get("month")}-${parts.get("day")}`;
}

/** 找到该时区下一个日期边界，兼容夏令时的 23/25 小时日。 */
export function millisecondsUntilNextDay(
  timeZone: string,
  now = Date.now(),
): number {
  const today = dateInTimeZone(timeZone, now);
  let low = now;
  let high = now + 30 * 60 * 60 * 1000;
  while (high - low > 1) {
    const middle = Math.floor((low + high) / 2);
    if (dateInTimeZone(timeZone, middle) === today) low = middle;
    else high = middle;
  }
  return high - now;
}
