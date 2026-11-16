#version 300 es
// one pass of a separable gaussian blur over a whole premultiplied texture
precision highp float;
in vec2 v_unit;
uniform sampler2D u_tex;
// one texel along the pass, in texture coordinates
uniform vec2 u_step;
// the standard deviation, in texels
uniform float u_sigma;
out vec4 frag;

void main() {
    vec2 uv = vec2(v_unit.x, 1.0 - v_unit.y);
    int reach = int(ceil(3.0 * u_sigma));
    float k = -0.5 / (u_sigma * u_sigma);
    vec4 sum = texture(u_tex, uv);
    float total = 1.0;
    // two taps a step, between texels, which linear filtering averages for free
    for (int i = 1; i <= reach; i += 2) {
        float w0 = exp(k * float(i * i));
        float w1 = exp(k * float((i + 1) * (i + 1)));
        float w = w0 + w1;
        float at = (float(i) * w0 + float(i + 1) * w1) / w;
        sum += (texture(u_tex, uv + u_step * at) + texture(u_tex, uv - u_step * at)) * w;
        total += 2.0 * w;
    }
    frag = sum / total;
}
