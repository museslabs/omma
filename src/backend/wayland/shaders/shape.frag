#version 300 es
// a rounded rectangle with a border, or the soft shadow of one
precision highp float;
in vec2 v_local;
uniform vec2 u_size;
uniform float u_pad;
uniform float u_radius;
uniform vec4 u_fill;
uniform vec4 u_border;
uniform float u_border_width;
// the shadow's softness, or 0 for the shape itself
uniform float u_shadow;
out vec4 frag;

float rounded_box(vec2 p, vec2 half_size, float r) {
    vec2 q = abs(p) - half_size + r;
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}

// erf, close enough for a shadow
float erf(float x) {
    float s = sign(x);
    float a = abs(x);
    float t = 1.0 + (0.278393 + (0.230389 + 0.078108 * a * a) * a) * a;
    t *= t;
    return s - s / (t * t);
}

void main() {
    vec2 p = v_local - u_pad - u_size * 0.5;
    if (u_shadow > 0.0) {
        float d = rounded_box(p, u_size * 0.5, u_radius);
        float cover = 0.5 - 0.5 * erf(d / (u_shadow * 1.41421356));
        frag = u_fill * cover;
        return;
    }
    float d = rounded_box(p, u_size * 0.5, u_radius);
    float aa = max(fwidth(d), 1e-4);
    float outer = clamp(0.5 - d / aa, 0.0, 1.0);
    float inner = clamp(0.5 - (d + u_border_width) / aa, 0.0, 1.0);
    frag = u_fill * inner + u_border * (outer - inner);
}
