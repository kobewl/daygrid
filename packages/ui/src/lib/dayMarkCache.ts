import type { DayMark } from "../api";
import type { DayKind, MarkValue } from "./workday";

type Transport = {
  list: (from: string, to: string) => Promise<DayMark[]>;
  set: (date: string, value: MarkValue) => Promise<unknown>;
};

/** 串行读取与写入；界面显示已确认的数据加上尚未完成的用户选择。 */
export class DayMarkCache {
  private confirmed = new Map<string, DayKind>();
  private pending: { date: string; value: MarkValue }[] = [];
  private queue: Promise<unknown> = Promise.resolve();

  constructor(
    private transport: Transport,
    private changed: (marks: ReadonlyMap<string, DayKind>) => void,
  ) {}

  private publish() {
    const marks = new Map(this.confirmed);
    for (const { date, value } of this.pending) {
      if (value === "default") marks.delete(date);
      else marks.set(date, value);
    }
    this.changed(marks);
  }

  private enqueue<T>(run: () => Promise<T>): Promise<T> {
    const result = this.queue.then(run);
    this.queue = result.catch(() => undefined);
    return result;
  }

  load(from: string, to: string) {
    return this.enqueue(async () => {
      const marks = await this.transport.list(from, to);
      for (const date of this.confirmed.keys()) {
        if (date >= from && date <= to) this.confirmed.delete(date);
      }
      for (const mark of marks) this.confirmed.set(mark.date, mark.kind);
      this.publish();
    });
  }

  set(date: string, value: MarkValue) {
    const operation = { date, value };
    this.pending.push(operation);
    this.publish();
    return this.enqueue(async () => {
      try {
        await this.transport.set(date, value);
        if (value === "default") this.confirmed.delete(date);
        else this.confirmed.set(date, value);
      } finally {
        // 失败只移除这一笔乐观选择，后续选择仍覆盖已确认数据。
        this.pending = this.pending.filter((p) => p !== operation);
        this.publish();
      }
    });
  }
}
