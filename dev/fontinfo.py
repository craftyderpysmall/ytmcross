"""Prints a font's tables (with sizes), glyph and codepoint counts."""
import sys
from fontTools.ttLib import TTFont

for path in sys.argv[1:]:
    f = TTFont(path, lazy=True)
    print(path)
    print("  glyphs:", len(f.getGlyphOrder()), " cmap codepoints:", len(f.getBestCmap()))
    tables = sorted(((f.reader.tables[t].length, t) for t in f.reader.keys()), reverse=True)
    print("  " + ", ".join(f"{t}={n // 1024}K" for n, t in tables if n >= 1024))
    print("  small:", ", ".join(t for n, t in tables if n < 1024))
