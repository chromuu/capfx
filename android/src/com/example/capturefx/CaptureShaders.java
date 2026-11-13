package com.example.capturefx;

/**
 * GLSL ES 3.00 sources for the capture-card pipeline.
 * Generated from the tested WebGL2 version (index.html) by gen_shaders.py. Don't hand-edit.
 */
final class CaptureShaders {
    private CaptureShaders() {}

    static final String VS =
        "#version 300 es\n" +
        "void main(){ vec2 p = vec2((gl_VertexID<<1)&2, gl_VertexID&2); gl_Position = vec4(p*2.0-1.0,0.0,1.0); }";

    static final String HEAD =
        "#version 300 es\n" +
        "precision highp float; precision highp int; precision highp sampler2D;\n" +
        "out vec4 o;\n" +
        "const float PI = 3.14159265358979;\n" +
        "uint pcg(uint v){ uint s=v*747796405u+2891336453u; uint w=((s>>((s>>28u)+4u))^s)*277803737u; return (w>>22u)^w; }\n" +
        "float rnd(uint v){ return float(pcg(v))*(1.0/4294967296.0); }\n" +
        "const mat3 RGB2YIQ = mat3(0.299,0.596,0.211, 0.587,-0.274,-0.523, 0.114,-0.322,0.312);\n" +
        "const mat3 YIQ2RGB = mat3(1.0,1.0,1.0, 0.956,-0.272,-1.106, 0.621,-0.647,1.703);\n" +
        "// subcarrier phase in quarter cycles at 4fsc: 227.5 cycles/line (+2 per field line),\n" +
        "// 262.5-line field offset (+3), 1/2 cycle per frame (+2)\n" +
        "int quarter(int x, int line, int frame){ return (x + 2*(line>>1) + 3*(line&1) + 2*(frame&1)) & 3; }\n" +
        "";

    static final String FS_SRC =
HEAD +
        "\n" +
        "uniform sampler2D uA, uB;\n" +
        "void main(){\n" +
        "  float u = (gl_FragCoord.x - 8.0)/704.0, v = gl_FragCoord.y/480.0;\n" +
        "  vec3 c = vec3(0.0);\n" +
        "  if (u >= 0.0 && u <= 1.0) c = ((int(gl_FragCoord.y)&1)==0) ? texture(uA, vec2(u,v)).rgb : texture(uB, vec2(u,v)).rgb;\n" +
        "  o = vec4(c, 1.0);\n" +
        "}";

    static final String FS_ENC =
HEAD +
        "\n" +
        "uniform sampler2D uSrc;\n" +
        "uniform int uFrame; uniform float uTime, uNoise, uJitter, uHum, uBeat, uHue, uSetup, uSigY, uSigI, uSigQ;\n" +
        "vec3 srcAt(float xs, int line){\n" +
        "  float f = xs - 0.5; float fi = floor(f); int i0 = int(fi); float t = f - fi;\n" +
        "  vec3 a = texelFetch(uSrc, ivec2(clamp(i0,0,719), line), 0).rgb;\n" +
        "  vec3 b = texelFetch(uSrc, ivec2(clamp(i0+1,0,719), line), 0).rgb;\n" +
        "  return mix(a, b, t);\n" +
        "}\n" +
        "void main(){\n" +
        "  int x = int(gl_FragCoord.x), line = int(gl_FragCoord.y);\n" +
        "  const float SC = 720.0/764.0;\n" +
        "  uint lseed = uint(line)*7919u + uint(uFrame)*104729u;\n" +
        "  float jit = uJitter*(0.6*sin(float(line)*0.043 + uTime*2.7) + 0.8*(rnd(lseed)-0.5));\n" +
        "  float xs0 = (float(x) + 0.5 + jit)*SC;\n" +
        "  vec3 s = vec3(0.0), w = vec3(0.0);\n" +
        "  for (int k=-12; k<=12; k++){\n" +
        "    float kk = float(k);\n" +
        "    vec3 g = exp(-kk*kk/(2.0*vec3(uSigY*uSigY, uSigI*uSigI, uSigQ*uSigQ)));\n" +
        "    if (g.z < 1e-4) continue;\n" +
        "    vec3 yiq = RGB2YIQ*srcAt(xs0 + kk*SC, line);\n" +
        "    s += g*yiq; w += g;\n" +
        "  }\n" +
        "  vec3 yiq = s/w;\n" +
        "  float ph = float(quarter(x, line, uFrame))*0.5*PI + uHue;\n" +
        "  float sig = uSetup + (1.0-uSetup)*(yiq.x + yiq.y*cos(ph) + yiq.z*sin(ph));\n" +
        "  sig += uHum*0.07*sin(2.0*PI*(float(line)/480.0 + uTime*0.06));\n" +
        "  sig += uBeat*0.035*sin(2.0*PI*(float(x)*0.093 + float(line)*0.31 + uTime*1.7));\n" +
        "  uint ns = uint(x)*73856093u ^ uint(line)*19349663u ^ uint(uFrame)*83492791u;\n" +
        "  float u1 = max(rnd(ns), 1e-7), u2 = rnd(ns ^ 0x9e3779b9u);\n" +
        "  sig += uNoise*sqrt(-2.0*log(u1))*cos(2.0*PI*u2);\n" +
        "  o = vec4(sig, 0.0, 0.0, 1.0);\n" +
        "}";

    static final String FS_YC1 =
HEAD +
        "\n" +
        "uniform sampler2D uComp; uniform int uMode;\n" +
        "float S(int x, int l){ return texelFetch(uComp, ivec2(clamp(x,0,763), clamp(l,0,479)), 0).r; }\n" +
        "float rawC(int x, int l){\n" +
        "  if (uMode == 0) return S(x,l);                                  // notch: whole signal\n" +
        "  if (uMode == 1) return 0.5*(S(x,l) - S(x,l+2));                 // 2-line comb (same field)\n" +
        "  return 0.25*(2.0*S(x,l) - S(x,l-2) - S(x,l+2));                 // 3-line comb\n" +
        "}\n" +
        "void main(){\n" +
        "  int x = int(gl_FragCoord.x), l = int(gl_FragCoord.y);\n" +
        "  float c0;\n" +
        "  if (uMode == 0) c0 = 0.25*(2.0*rawC(x,l) - rawC(x-2,l) - rawC(x+2,l));   // zero-DC bandpass, narrowed in pass 3b\n" +
        "  else c0 = rawC(x,l) - (6.0*rawC(x,l) + 4.0*(rawC(x-2,l) + rawC(x+2,l)) + rawC(x-4,l) + rawC(x+4,l))/16.0; // flat-top 1-cos^4 band\n" +
        "  o = vec4(S(x,l), c0, 0.0, 1.0);\n" +
        "}";

    static final String FS_YC2 =
HEAD +
        "\n" +
        "uniform sampler2D uT; uniform float uSigB;\n" +
        "void main(){\n" +
        "  int x = int(gl_FragCoord.x), l = int(gl_FragCoord.y);\n" +
        "  vec4 t0 = texelFetch(uT, ivec2(x,l), 0); float s = t0.r;\n" +
        "  if (uSigB <= 0.0){ o = vec4(s - t0.g, t0.g, s, 1.0); return; }   // comb modes: band already set in 3a\n" +
        "  float c = 0.0, n = 0.0;\n" +
        "  for (int k=-12; k<=12; k+=2){\n" +
        "    float kk = float(k); float g = exp(-kk*kk/(2.0*uSigB*uSigB));\n" +
        "    float sg = ((k/2)&1)==0 ? 1.0 : -1.0;\n" +
        "    c += g*sg*texelFetch(uT, ivec2(clamp(x+k,0,763), l), 0).g; n += g;\n" +
        "  }\n" +
        "  c /= n;\n" +
        "  o = vec4(s - c, c, s, 1.0);\n" +
        "}";

    static final String FS_DEM =
HEAD +
        "\n" +
        "uniform sampler2D uYC; uniform int uFrame;\n" +
        "uniform float uSigD, uPeak, uSat, uTint, uSetupDec;\n" +
        "float Yat(float xc, int l){\n" +
        "  float f = floor(xc); int i = int(f); float t = xc - f;\n" +
        "  return mix(texelFetch(uYC, ivec2(clamp(i,0,763),l),0).r, texelFetch(uYC, ivec2(clamp(i+1,0,763),l),0).r, t);\n" +
        "}\n" +
        "void main(){\n" +
        "  int x = int(gl_FragCoord.x), l = int(gl_FragCoord.y);\n" +
        "  float xc = (float(x)+0.5)*(764.0/720.0) - 0.5;\n" +
        "  float Y = Yat(xc,l);\n" +
        "  Y += uPeak*(Y - 0.5*(Yat(xc-1.7,l) + Yat(xc+1.7,l)));\n" +
        "  int n = int(floor(xc + 0.5));\n" +
        "  vec2 iq = vec2(0.0); float W = 0.0;\n" +
        "  for (int k=-12; k<=12; k++){\n" +
        "    int xi = clamp(n+k, 0, 763);\n" +
        "    float d = float(n+k) - xc; float g = exp(-d*d/(2.0*uSigD*uSigD));\n" +
        "    float c = texelFetch(uYC, ivec2(xi,l), 0).g;\n" +
        "    float ph = float(quarter(xi, l, uFrame))*0.5*PI + uTint;\n" +
        "    iq += g*c*2.0*vec2(cos(ph), sin(ph)); W += g;\n" +
        "  }\n" +
        "  iq /= W;\n" +
        "  float k = 1.0/(1.0 - uSetupDec);\n" +
        "  vec3 rgb = YIQ2RGB*vec3((Y - uSetupDec)*k, iq*k*uSat);\n" +
        "  o = vec4(clamp(rgb, 0.0, 1.0), 1.0);\n" +
        "}";

    static final String FS_TOY =
HEAD +
        "\n" +
        "uniform sampler2D uT;\n" +
        "void main(){\n" +
        "  vec3 c = texelFetch(uT, ivec2(gl_FragCoord.xy), 0).rgb;\n" +
        "  float y = (16.0 + 219.0*dot(c, vec3(0.299,0.587,0.114)))/255.0;\n" +
        "  o = vec4(floor(y*255.0+0.5)/255.0, 0.0, 0.0, 1.0);\n" +
        "}";

    static final String FS_TOC =
HEAD +
        "\n" +
        "uniform sampler2D uT;\n" +
        "void main(){\n" +
        "  int cx = int(gl_FragCoord.x), cr = int(gl_FragCoord.y);\n" +
        "  int p = cr & 1, j = cr >> 1; int r0 = 4*j + p, r1 = r0 + 2;\n" +
        "  vec3 c = 0.25*(texelFetch(uT, ivec2(2*cx, r0),0).rgb + texelFetch(uT, ivec2(2*cx+1, r0),0).rgb\n" +
        "               + texelFetch(uT, ivec2(2*cx, r1),0).rgb + texelFetch(uT, ivec2(2*cx+1, r1),0).rgb);\n" +
        "  float y = dot(c, vec3(0.299,0.587,0.114));\n" +
        "  vec2 cbcr = vec2((c.b - y)*0.564, (c.r - y)*0.713);\n" +
        "  cbcr = floor((128.0 + 224.0*cbcr) + 0.5)/255.0;\n" +
        "  o = vec4(cbcr, 0.0, 1.0);\n" +
        "}";

    static final String FS_DCT =
HEAD +
        "\n" +
        "uniform sampler2D uT; uniform int uDir, uInv, uQuant; uniform float uQ;\n" +
        "const float QM[64] = float[64](\n" +
        "  8.,16.,19.,22.,26.,27.,29.,34.,  16.,16.,22.,24.,27.,29.,34.,37.,\n" +
        "  19.,22.,26.,27.,29.,34.,34.,38., 22.,22.,26.,27.,29.,34.,37.,40.,\n" +
        "  22.,26.,27.,29.,32.,35.,40.,48., 26.,27.,29.,32.,35.,40.,48.,58.,\n" +
        "  26.,27.,29.,34.,38.,46.,56.,69., 27.,29.,35.,38.,46.,56.,69.,83.);\n" +
        "float cf(int k){ return k==0 ? 0.35355339 : 0.5; }\n" +
        "void main(){\n" +
        "  ivec2 p = ivec2(gl_FragCoord.xy);\n" +
        "  int pos = uDir==0 ? p.x : p.y; int i = pos & 7; int base = pos - i;\n" +
        "  vec4 acc = vec4(0.0);\n" +
        "  for (int n=0; n<8; n++){\n" +
        "    ivec2 q = uDir==0 ? ivec2(base+n, p.y) : ivec2(p.x, base+n);\n" +
        "    vec4 v = texelFetch(uT, q, 0);\n" +
        "    acc += uInv==0 ? v*cf(i)*cos(float((2*n+1)*i)*PI/16.0)\n" +
        "                   : v*cf(n)*cos(float((2*i+1)*n)*PI/16.0);\n" +
        "  }\n" +
        "  if (uQuant == 1){\n" +
        "    int u = p.x & 7, vv = p.y & 7;\n" +
        "    vec4 F = acc*255.0;\n" +
        "    if (u == 0 && vv == 0) F = floor(F/8.0 + 0.5)*8.0; // 8-bit intra DC precision\n" +
        "    else {\n" +
        "      float st = QM[vv*8+u]*uQ/16.0;\n" +
        "      F = sign(F)*max(floor(abs(F)/st + 0.4), 0.0)*st;  // slight dead zone, as encoders did\n" +
        "    }\n" +
        "    acc = F/255.0;\n" +
        "  }\n" +
        "  o = acc;\n" +
        "}";

    static final String FS_OUT =
HEAD +
        "\n" +
        "uniform sampler2D uY, uC; uniform int uCUE, uDeint, uFrame;\n" +
        "vec2 C(int cx, int cr){ return texelFetch(uC, ivec2(clamp(cx,0,359), clamp(cr,0,239)), 0).rg; }\n" +
        "vec2 chromaRow(float cxf, int cr){\n" +
        "  float f = floor(cxf); int i = int(f); return mix(C(i,cr), C(i+1,cr), cxf - f);\n" +
        "}\n" +
        "vec3 pix(int x, int l){\n" +
        "  l = clamp(l, 0, 479);\n" +
        "  float Y = texelFetch(uY, ivec2(x,l), 0).r;\n" +
        "  float cxf = (float(x) - 0.5)*0.5;\n" +
        "  vec2 cc;\n" +
        "  if (uCUE == 1){\n" +
        "    float r = (float(l) - 0.5)*0.5; float f = floor(r);\n" +
        "    cc = mix(chromaRow(cxf, int(f)), chromaRow(cxf, int(f)+1), r - f);\n" +
        "  } else {\n" +
        "    int p = l & 1, m = l >> 1;\n" +
        "    float r = (float(m) - 0.5)*0.5; float f = floor(r); int j = int(f);\n" +
        "    cc = mix(chromaRow(cxf, 2*j+p), chromaRow(cxf, 2*(j+1)+p), r - f);\n" +
        "  }\n" +
        "  float yf = (Y*255.0 - 16.0)/219.0; vec2 c = (cc*255.0 - 128.0)/224.0;\n" +
        "  return vec3(yf + 1.402*c.y, yf - 0.344136*c.x - 0.714136*c.y, yf + 1.772*c.x);\n" +
        "}\n" +
        "void main(){\n" +
        "  int x = int(gl_FragCoord.x), l = int(gl_FragCoord.y);\n" +
        "  vec3 c;\n" +
        "  if (uDeint == 1){ int p = uFrame & 1; c = ((l&1)==p) ? pix(x,l) : 0.5*(pix(x,l-1)+pix(x,l+1)); }\n" +
        "  else if (uDeint == 2) c = 0.5*(pix(x,l) + pix(x, l^1));\n" +
        "  else c = pix(x,l);\n" +
        "  o = vec4(clamp(c,0.0,1.0), 1.0);\n" +
        "}";

    static final String FS_PRE =
HEAD +
        "\n" +
        "uniform sampler2D uT, uRef; uniform int uKind; uniform float uOver, uSplit; uniform vec2 uCanvas, uOrigin;\n" +
        "vec4 bil(sampler2D t, vec2 uv){\n" +
        "  ivec2 sz = textureSize(t,0); vec2 f = uv*vec2(sz) - 0.5; vec2 fl = floor(f); ivec2 i = ivec2(fl); vec2 w = f - fl;\n" +
        "  ivec2 m = sz - 1;\n" +
        "  vec4 a = texelFetch(t, clamp(i,ivec2(0),m),0), b = texelFetch(t, clamp(i+ivec2(1,0),ivec2(0),m),0);\n" +
        "  vec4 c = texelFetch(t, clamp(i+ivec2(0,1),ivec2(0),m),0), d = texelFetch(t, clamp(i+ivec2(1,1),ivec2(0),m),0);\n" +
        "  return mix(mix(a,b,w.x), mix(c,d,w.x), w.y);\n" +
        "}\n" +
        "void main(){\n" +
        "  vec2 fc = gl_FragCoord.xy - uOrigin;\n" +
        "  vec2 uv0 = fc/uCanvas; vec2 uv = mix(vec2(uOver), vec2(1.0-uOver), uv0);\n" +
        "  vec4 v = bil(uT, uv); vec3 c;\n" +
        "  if (uKind == 1) c = vec3(clamp(v.r/1.3, 0.0, 1.0));\n" +
        "  else if (uKind == 2) c = vec3(clamp(v.r, 0.0, 1.0));\n" +
        "  else if (uKind == 3) c = vec3(clamp(0.5 + v.g*1.4, 0.0, 1.0));\n" +
        "  else c = v.rgb;\n" +
        "  if (uSplit >= 0.0){\n" +
        "    float dx = fc.x - uSplit*uCanvas.x;\n" +
        "    if (dx < 0.0) c = bil(uRef, uv).rgb;\n" +
        "    if (abs(dx) < 1.5) c = vec3(0.89, 0.63, 0.26);\n" +
        "  }\n" +
        "  o = vec4(c, 1.0);\n" +
        "}";

    static final String FS_COPY_2D =
        "#version 300 es\n" +
        "precision highp float;\n" +
        "uniform sampler2D uIn;\n" +
        "uniform mat4 uTexM; uniform vec2 uFit;\n" +
        "out vec4 o;\n" +
        "void main(){\n" +
        "  vec2 f = (gl_FragCoord.xy/vec2(640.0, 480.0) - 0.5)/uFit + 0.5;\n" +
        "  vec3 c = vec3(0.0);\n" +
        "  if (f.x >= 0.0 && f.x <= 1.0 && f.y >= 0.0 && f.y <= 1.0) c = texture(uIn, (uTexM*vec4(f, 0.0, 1.0)).xy).rgb;\n" +
        "  o = vec4(c, 1.0);\n" +
        "}";

    static final String FS_COPY_OES =
        "#version 300 es\n" +
        "#extension GL_OES_EGL_image_external_essl3 : require\n" +
        "precision highp float;\n" +
        "uniform samplerExternalOES uIn;\n" +
        "uniform mat4 uTexM; uniform vec2 uFit;\n" +
        "out vec4 o;\n" +
        "void main(){\n" +
        "  vec2 f = (gl_FragCoord.xy/vec2(640.0, 480.0) - 0.5)/uFit + 0.5;\n" +
        "  vec3 c = vec3(0.0);\n" +
        "  if (f.x >= 0.0 && f.x <= 1.0 && f.y >= 0.0 && f.y <= 1.0) c = texture(uIn, (uTexM*vec4(f, 0.0, 1.0)).xy).rgb;\n" +
        "  o = vec4(c, 1.0);\n" +
        "}";
}
