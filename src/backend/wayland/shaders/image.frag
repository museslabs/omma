#version 300 es
// a premultiplied texture, multiplied by a premultiplied tint
precision highp float;
in vec2 v_unit;
uniform sampler2D u_tex;
// the part of the texture to show: left, top, right, bottom
uniform vec4 u_uv;
uniform vec4 u_tint;
out vec4 frag;

void main() {
    frag = texture(u_tex, mix(u_uv.xy, u_uv.zw, v_unit)) * u_tint;
}
