"""Generate CaptureShaders.java from the tested WebGL2 page so the GLSL stays identical."""
import re, sys, pathlib

root = pathlib.Path(__file__).resolve().parent
html = (root.parent / "index.html").read_text()

pat = re.compile(r"const (VS|HEAD|FS_[A-Z0-9]+) = (HEAD \+ )?`([\s\S]*?)`;")
found = pat.findall(html)
names = [n for n, _, _ in found]
assert names[:2] == ["VS", "HEAD"] and len(found) == 12, names

def jstr(src: str, indent="        ") -> str:
    lines = src.split("\n")
    out = []
    for i, line in enumerate(lines):
        esc = line.replace("\\", "\\\\").replace('"', '\\"')
        nl = "\\n" if i < len(lines) - 1 else ""
        out.append(f'{indent}"{esc}{nl}"')
    return " +\n".join(out)

# Copy pass (Android only): letterboxes any input into the 640x480 console frame.
COPY_BODY = """
uniform mat4 uTexM; uniform vec2 uFit;
out vec4 o;
void main(){
  vec2 f = (gl_FragCoord.xy/vec2(640.0, 480.0) - 0.5)/uFit + 0.5;
  vec3 c = vec3(0.0);
  if (f.x >= 0.0 && f.x <= 1.0 && f.y >= 0.0 && f.y <= 1.0) c = texture(uIn, (uTexM*vec4(f, 0.0, 1.0)).xy).rgb;
  o = vec4(c, 1.0);
}"""
COPY_2D = "#version 300 es\nprecision highp float;\nuniform sampler2D uIn;" + COPY_BODY
COPY_OES = ("#version 300 es\n#extension GL_OES_EGL_image_external_essl3 : require\nprecision highp float;\n"
            "uniform samplerExternalOES uIn;" + COPY_BODY)

parts = []
for name, plus_head, body in found:
    expr = ("HEAD +\n" if plus_head else "") + jstr(body)
    parts.append(f"    static final String {name} =\n{expr};\n")
parts.append(f"    static final String FS_COPY_2D =\n{jstr(COPY_2D)};\n")
parts.append(f"    static final String FS_COPY_OES =\n{jstr(COPY_OES)};\n")

java = f"""package com.example.capturefx;

/**
 * GLSL ES 3.00 sources for the capture-card pipeline.
 * Generated from the tested WebGL2 version (index.html) by gen_shaders.py. Don't hand-edit.
 */
final class CaptureShaders {{
    private CaptureShaders() {{}}

{chr(10).join(parts)}}}
"""
out = root / "src" / "com" / "example" / "capturefx" / "CaptureShaders.java"
out.parent.mkdir(parents=True, exist_ok=True)
out.write_text(java)
print("wrote", out, "with", len(found) + 2, "shader constants")
