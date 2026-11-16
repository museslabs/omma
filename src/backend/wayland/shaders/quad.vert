#version 300 es
// a quad over u_rect, in logical pixels with y down, through u_xform, onto the target that
// shows the logical area u_view
precision highp float;
layout(location = 0) in vec2 a_unit;
uniform vec4 u_rect;
uniform mat3 u_xform;
uniform vec4 u_view;
out vec2 v_local;
out vec2 v_unit;

void main() {
    vec2 p = u_rect.xy + a_unit * u_rect.zw;
    vec2 q = (u_xform * vec3(p, 1.0)).xy;
    vec2 ndc = (q - u_view.xy) / u_view.zw * 2.0 - 1.0;
    gl_Position = vec4(ndc.x, -ndc.y, 0.0, 1.0);
    v_local = a_unit * u_rect.zw;
    v_unit = a_unit;
}
