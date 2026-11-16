// the mati bead, drawn whole in one pass, or in parts so its pigment can blur on its own.
// its numbers come from `mati`, as #defines put ahead of this source. coordinates are
// logical pixels from the bead's center with y up, as the macOS backend's layers have them.
precision highp float;
in vec2 v_local;
uniform float u_d;
uniform float u_pad;
// which parts to draw, as bits: shadow and glow, the solid disc, the pigment, the glass, the eye
uniform int u_parts;
uniform float u_solid;
uniform float u_counts;
// the drift's phase (0…τ) and how far it is swollen from a circle
uniform vec2 u_drift;
// how far the top is pressed flat, as a share of the radius
uniform float u_flat;
// how far each disc leans toward the pointer, in logical pixels with y up
uniform vec2 u_look[3];
// the pupil's scale across and up, for its blink
uniform vec2 u_blink;
// how annoyed the lid is (0…1), and which way it slopes
uniform vec2 u_lid;
// where the liquid's wave crests sit, up from the bead's bottom, and the wave's position
uniform float u_level;
uniform float u_wave;
// the gradients' colors, straight RGB and alpha, and where each stop sits
uniform vec4 u_body[5];
uniform float u_body_at[5];
uniform vec4 u_white[5];
uniform vec4 u_sky[5];
uniform float u_rings_at[5];
uniform vec4 u_pupil;
// premultiplied
uniform vec4 u_glow;
out vec4 frag;

const float TAU = 6.28318530718;

float r;
vec2 p;

float soft_min(float a, float b, float k) {
    float h = max(k - abs(a - b), 0.0) / k;
    return min(a, b) - h * h * k / 4.0;
}

float pigment_radius(float angle) {
    float phase = u_drift.x;
    float two = (0.50 + 0.12 * sin(2.0 * phase + 0.7)) * sin(2.0 * angle + phase);
    float three = (0.38 + 0.10 * cos(phase)) * sin(3.0 * angle - phase + 1.3);
    return LIQUID + MORPH * u_drift.y * (two + three);
}

float glass_radius(float angle) {
    return GLASS + GLASS_MORPH * (pigment_radius(angle) - LIQUID);
}

// the signed distance in pixels from `at` to an outline: 0 pigment, 1 glass, 2 rim. a press
// cuts its top flat below where it sits at rest, and rounds the cut into the curve
float outline(vec2 at, int shape) {
    vec2 q = at / r;
    float angle = atan(q.y, q.x);
    float reach = shape == 0 ? pigment_radius(angle) : shape == 1 ? glass_radius(angle) : 1.0;
    float top = shape == 0 ? LIQUID : shape == 1 ? GLASS : 1.0;
    float d = (length(q) - reach) * r;
    if (u_flat > 0.0) {
        float cut = (q.y - (top - u_flat)) * r;
        d = -soft_min(-d, -cut, r * min(u_flat, FLAT_ROUNDING));
    }
    return d;
}

float cover(float d) {
    float aa = max(fwidth(d), 1e-3);
    return clamp(0.5 - d / aa, 0.0, 1.0);
}

vec4 ramp(vec4 colours[5], float at[5], float t) {
    t = clamp(t, 0.0, 1.0);
    vec4 colour = colours[0];
    for (int i = 0; i < 4; i++) {
        if (t >= at[i]) {
            float f = clamp((t - at[i]) / max(at[i + 1] - at[i], 1e-5), 0.0, 1.0);
            colour = mix(colours[i], colours[i + 1], f);
        }
    }
    return colour;
}

// a radial gradient that is lightest at `light`, in the unit square of a disc with y up. it
// reaches its last stop at the far side of the circle in that square
vec4 dome(vec4 colours[5], float at[5], vec2 light, vec2 unit) {
    float reach = 0.5 + length(light - 0.5);
    return ramp(colours, at, length(unit - light) / reach);
}

vec4 premul(vec4 straight, float coverage) {
    float a = straight.a * coverage;
    return vec4(straight.rgb * a, a);
}

vec4 over(vec4 top, vec4 under) {
    return top + under * (1.0 - top.a);
}

// whether a point is below the liquid's surface, for the beads that count down
float liquid() {
    if (u_counts < 0.5) {
        return 1.0;
    }
    vec2 b = p + r;
    float swell = u_d * WAVE_HEIGHT;
    float x = b.x - (u_wave - WAVE_SPAN * u_d / 2.0);
    float surface = u_level - swell + swell * sin(TAU * x / (u_d * WAVE_LENGTH));
    return cover(b.y - surface);
}

void main() {
    r = u_d / 2.0;
    p = v_local - vec2(u_pad + r);
    p.y = -p.y;
    bool solid = u_solid > 0.5;
    vec4 colour = vec4(0.0);
    // the glass of a bead that counts down drifts with its pigment. the glass of the bead that
    // hangs is a circle, cut flat only while pressed
    int glass_shape = u_counts > 0.5 ? 1 : 2;

    if ((u_parts & 1) != 0) {
        // a soft shadow under the glass, a little below it
        float below = outline(p + vec2(0.0, SHADOW_DROP * u_d), 1);
        float sigma = SHADOW_SOFTNESS * u_d;
        float shade = 0.5 - 0.5 * tanh(below / (sigma * 0.8));
        colour = vec4(0.0, 0.0, 0.0, SHADOW_OPACITY * shade);
        if (!solid) {
            // the glass lights the air around it with the pigment's blue
            float edge = outline(p, glass_shape);
            float glow = exp(-max(edge, 0.0) / (GLOW_REACH * u_d)) * (1.0 - cover(edge));
            colour = over(u_glow * glow, colour);
        }
    }

    if ((u_parts & 2) != 0 && solid) {
        float d = outline(p, 2);
        float inside = cover(d + 1.0);
        vec4 disc = premul(vec4(vec3(0.14), 1.0), inside);
        disc = over(premul(vec4(1.0, 1.0, 1.0, 0.35), cover(d) - inside), disc);
        colour = over(disc, colour);
    }

    if ((u_parts & 4) != 0) {
        vec4 body = dome(u_body, u_body_at, BODY_LIGHT, (p / r + 1.0) / 2.0);
        if (solid) {
            body.a = 1.0;
        }
        colour = over(premul(body, cover(outline(p, 0)) * liquid()), colour);
    }

    if ((u_parts & 8) != 0 && !solid) {
        float shell = outline(p, glass_shape);
        float inside = cover(shell);
        float body = cover(outline(p, 0));
        float wet = liquid();
        // the shell refracts the pigment beside it, so the clear ring around the pigment takes
        // on its blue while there is liquid to bend. drained, the glass is clear and frosted
        float ring = inside * (1.0 - body);
        float edge_alpha = 0.55 * ring * wet;
        vec4 glass = vec4(u_body[3].rgb * edge_alpha, edge_alpha);
        float frost = 0.04 * inside * body * wet + 0.09 * inside * (1.0 - wet * max(body, ring));
        glass = over(vec4(frost), glass);
        vec2 n = normalize(p + vec2(1e-4));
        // light falls from the upper left and glints again at the lower right
        float facing = dot(n, normalize(vec2(-0.6, 0.8)));
        float lit = 0.55 + 0.45 * facing;
        float glint = pow(max(-facing, 0.0), 3.0) * 0.6;
        float band = inside - cover(shell + RIM_WIDTH);
        glass = over(vec4(1.0) * band * (0.20 * lit + 0.22 * glint), glass);
        // a soft highlight up at the left, as on a glossy drop
        vec2 h = (p / r - vec2(-0.40, 0.50));
        float a = -0.62;
        vec2 hr = vec2(cos(a) * h.x - sin(a) * h.y, sin(a) * h.x + cos(a) * h.y);
        float spot = exp(-dot(hr / vec2(0.34, 0.13), hr / vec2(0.34, 0.13)));
        glass = over(vec4(1.0) * spot * 0.22 * inside, glass);
        vec2 g = p / r - vec2(0.42, -0.55);
        float low = exp(-dot(g / 0.2, g / 0.2));
        glass = over(vec4(1.0) * low * 0.10 * inside, glass);
        colour = over(glass, colour);
    }

    if ((u_parts & 16) != 0) {
        float iris_sink = IRIS_SINK * u_flat * r;
        float wet = liquid();
        vec4 rings[2];
        for (int i = 1; i >= 0; i--) {
            float radius = (i == 0 ? WHITE_RADIUS : SKY_RADIUS) * r;
            vec2 at = p - u_look[i] + vec2(0.0, iris_sink);
            vec2 unit = at / (2.0 * radius) + 0.5;
            vec4 ring = i == 0
                ? dome(u_white, u_rings_at, WHITE_LIGHT, unit)
                : dome(u_sky, u_rings_at, SKY_LIGHT, unit);
            if (solid) {
                ring.a = 1.0;
            }
            rings[i] = premul(ring, cover(length(at) - radius) * wet);
        }
        colour = over(rings[0], colour);
        colour = over(rings[1], colour);

        float side = 2.0 * PUPIL_RADIUS * r;
        vec2 socket = p - u_look[2] + vec2(0.0, PUPIL_SINK * u_flat * r);
        vec2 squash = max(u_blink, vec2(1e-3));
        float d = (length(socket / squash) - side / 2.0) * min(squash.x, squash.y);
        float shows = cover(d);
        if (u_lid.x > 0.0) {
            float edge = side * (LID_OPEN - u_lid.x * (LID_OPEN - 1.0 + LID_DROP)) - side / 2.0;
            float angle = u_lid.x * u_lid.y * LID_TILT;
            vec2 normal = vec2(-sin(angle), cos(angle));
            shows *= cover(dot(socket - vec2(0.0, edge), normal));
        }
        colour = over(premul(u_pupil, shows), colour);
    }

    frag = colour;
}
