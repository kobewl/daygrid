// 备注是 Markdown，但很多人会直接粘贴网址，而且常常紧挨着中文标点或文字：
//   两题（https://leetcode.cn/problems/fibonacci-number/）都已完成
// GFM 的裸网址自动识别会把后面的「）都已完成」一起吞进链接。
// 这里在交给 Markdown 之前，把裸网址按「只含 URL 合法 ASCII 字符」截出来，
// 包成 <网址>（CommonMark 的尖括号自动链接），边界就由我们说了算。
// 已经是 Markdown 链接、行内代码、围栏代码块、引用定义里的网址保持原样。

const URL_CHARS = "A-Za-z0-9\\-._~:/?#\\[\\]@!$&'()*+,;=%";
const INLINE = new RegExp(
  [
    "(`+)[^`]+\\1", //                   行内代码
    "!?\\[[^\\]]*\\]\\([^)]*\\)", //     [文字](网址) 与 ![alt](网址)
    "\\[[^\\]]*\\]\\[[^\\]]*\\]", //     [文字][引用]
    "<[^<>\\s]+>", //                    <网址> 或其他尖括号内容
    `\\bhttps?:\\/\\/[${URL_CHARS}]+`, // 裸网址
  ].join("|"),
  "gi",
);
const FENCE = /^\s{0,3}(```|~~~)/;
const DEFINITION = /^\s{0,3}\[[^\]]+\]:\s*\S/;

const count = (text: string, char: string) => text.split(char).length - 1;

/** 去掉网址末尾更可能属于句子的标点与不成对的括号。 */
function trimTail(url: string): [url: string, tail: string] {
  let end = url.length;
  for (;;) {
    const c = url[end - 1];
    const head = url.slice(0, end);
    if (c !== undefined && ".,;:!?'*_~".includes(c)) end--;
    else if (c === ")" && count(head, ")") > count(head, "(")) end--;
    else if (c === "]" && count(head, "]") > count(head, "[")) end--;
    else break;
  }
  return [url.slice(0, end), url.slice(end)];
}

export function linkifyBareUrls(source: string): string {
  let fenced = false;
  return source
    .split("\n")
    .map((line) => {
      if (FENCE.test(line)) {
        fenced = !fenced;
        return line;
      }
      if (fenced || DEFINITION.test(line)) return line;
      return line.replace(INLINE, (match) => {
        if (!/^https?:\/\//i.test(match)) return match;
        const [url, tail] = trimTail(match);
        // 光秃秃的 "https://" 不是链接。
        return /^https?:\/\/[^/?#]+/i.test(url) && url.length > 8
          ? `<${url}>${tail}`
          : match;
      });
    })
    .join("\n");
}
