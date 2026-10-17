"""校验 GitHub Actions workflow 的 YAML 能否解析。

为什么单独成文件，而不是在 check-ci.js 里用 `python -c "…"`：
  1. Windows 的参数解析会吃掉内层引号 —— 实测 execFileSync 传数组参数时，
     python3 明明存在却被判成「解析失败」，而真实原因跟 YAML 无关。
  2. 更要紧的是**转义层数**。把 Python 源码塞进 JS 字符串字面量，我在一轮里
     连续踩了三次：漏引号（`sys.exit(4)` 被当成 JS 执行）、把 Python 的
     `chr(10)` 留在 JS 里、以及单双引号嵌套导致的 SyntaxError。
     每一次都是「生成器脚本自己出错」，而症状出现在 check-ci.js 里 ——
     排查方向被带偏。

独立文件把这整类问题消掉了：Python 在 .py 里，JS 在 .js 里，各自用各自的语法。

退出码：
  0  YAML 可解析
  3  YAML 语法错误（stdout 打印 YAMLERR line N: <原因>）
  4  文件读不到（stdout 打印 IOERR: <原因>）
  9  没装 PyYAML（调用方据此换 js-yaml，而不是当成「校验通过」）
"""
import sys


def main() -> int:
    if len(sys.argv) < 2:
        print("usage: yamlcheck.py <file>")
        return 4
    try:
        import yaml
    except ModuleNotFoundError:
        print("NOPYyaml")
        return 9
    try:
        with open(sys.argv[1], encoding="utf-8") as f:
            yaml.safe_load(f)
    except yaml.YAMLError as e:
        mark = getattr(e, "problem_mark", None)
        line = (mark.line + 1) if mark else "?"
        problem = getattr(e, "problem", None) or str(e)
        print("YAMLERR line %s: %s" % (line, problem))
        return 3
    except OSError as e:
        print("IOERR: %s" % e)
        return 4
    return 0


if __name__ == "__main__":
    sys.exit(main())
