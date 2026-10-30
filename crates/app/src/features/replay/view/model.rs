use super::*;
use glam::{Quat, Vec3};
use slimenrf_recording::replay::{
    ReplayFrame,
    analysis::{linear_acceleration_body, magnetic_body},
};

const MAX_VERTICES: usize = 192;
const MAX_FACES: usize = 128;
const DEFAULT_AZIMUTH: f32 = -0.9;
const DEFAULT_ELEVATION: f32 = 0.58;
const DEFAULT_DISTANCE: f32 = 3.8;
const DIRECTION_LENGTH: f32 = 1.06;
const KEYBOARD_ORBIT_STEP: f32 = 0.1;
const KEYBOARD_ZOOM_FACTOR: f32 = 0.9;

actions!(
    replay_model,
    [
        OrbitLeft,
        OrbitRight,
        OrbitUp,
        OrbitDown,
        ZoomIn,
        ZoomOut,
        ResetCamera
    ]
);

fn normalized_direction(vector: [f32; 3]) -> Option<Vec3> {
    let vector = Vec3::from_array(vector);
    if !vector.is_finite() {
        return None;
    }
    let scale = vector.abs().max_element();
    if scale == 0. {
        return None;
    }
    // Scale before taking the norm: both tiny and large genuine observations
    // keep their direction without an underflowed/overflowed squared length.
    // Explicit scalar divisions also avoid a SIMD reciprocal overflowing for
    // subnormal input before multiplication.
    Some(Vec3::new(vector.x / scale, vector.y / scale, vector.z / scale).normalize())
}

#[derive(Clone, Copy, Default)]
struct Face {
    indices: [usize; 16],
    len: usize,
    normal: Vec3,
    center: Vec3,
    material: usize,
    mounting_plane: Option<(Vec3, f32)>,
}

struct Mesh {
    vertices: [Vec3; MAX_VERTICES],
    vertex_count: usize,
    faces: [Face; MAX_FACES],
    face_count: usize,
}

impl Mesh {
    fn ring<const N: usize>(&mut self, points: [Vec3; N]) -> [usize; N] {
        let start = self.vertex_count;
        self.vertices[start..start + N].copy_from_slice(&points);
        self.vertex_count += N;
        std::array::from_fn(|i| start + i)
    }

    fn face(&mut self, indices: &[usize], material: usize) {
        let mut face = Face {
            len: indices.len(),
            material,
            ..Face::default()
        };
        face.indices[..indices.len()].copy_from_slice(indices);
        for &i in indices {
            face.center += self.vertices[i];
        }
        face.center /= indices.len() as f32;
        face.normal = (self.vertices[indices[1]] - self.vertices[indices[0]])
            .cross(self.vertices[indices[2]] - self.vertices[indices[0]])
            .normalize();
        self.faces[self.face_count] = face;
        self.face_count += 1;
    }

    fn join<const N: usize>(&mut self, lower: [usize; N], upper: [usize; N], material: usize) {
        for i in 0..N {
            let j = (i + 1) % N;
            self.face(&[lower[i], lower[j], upper[j], upper[i]], material);
        }
    }

    fn mount_since(&mut self, start: usize, normal: Vec3, distance: f32) {
        for face in &mut self.faces[start..self.face_count] {
            face.mounting_plane = Some((normal, distance));
        }
    }

    fn housing() -> Self {
        let mut mesh = Self {
            vertices: [Vec3::ZERO; MAX_VERTICES],
            vertex_count: 0,
            faces: [Face::default(); MAX_FACES],
            face_count: 0,
        };
        // Lightweight assembled appearance inspired by the supplied case flat v5 STL:
        // 37 x 34.3 mm footprint, rounded lid, molded button and lower-edge channels.
        // The reference contains separated print parts; it is not imported as a live mesh.
        let mut previous = mesh.ring(rounded_rectangle(0.705, 0.651, 0.115, -0.205));
        let mut bottom = previous;
        bottom.reverse();
        mesh.face(&bottom, 1);
        for (x, y, radius, z, material) in [
            (0.74, 0.686, 0.14, -0.165, 1),
            (0.74, 0.686, 0.14, 0.115, 1),
            (0.74, 0.686, 0.14, 0.127, 2),
            (0.74, 0.686, 0.14, 0.155, 0),
            (0.715, 0.661, 0.115, 0.205, 0),
        ] {
            let next = mesh.ring(rounded_rectangle(x, y, radius, z));
            mesh.join(previous, next, material);
            previous = next;
        }
        mesh.face(&previous, 0);

        let button_start = mesh.face_count;
        let button_base = mesh.ring(circle(0.13, 0.206).map(|p| p + Vec3::new(0.40, 0.02, 0.)));
        let button_top = mesh.ring(circle(0.117, 0.213).map(|p| p + Vec3::new(0.40, 0.02, 0.)));
        mesh.join(button_base, button_top, 2);
        mesh.face(&button_top, 0);
        // Two short slit edges retain the reference's molded button stem.
        for y in [-0.028, 0.062] {
            let slit = mesh.ring([
                Vec3::new(0.17, y, 0.206),
                Vec3::new(0.282, y, 0.206),
                Vec3::new(0.282, y + 0.006, 0.206),
                Vec3::new(0.17, y + 0.006, 0.206),
            ]);
            mesh.face(&slit, 2);
        }
        mesh.mount_since(button_start, Vec3::Z, 0.205);

        // USB-shaped opening on the positive-X end, as in the appearance reference.
        let port_start = mesh.face_count;
        let port_outer = mesh.ring(
            octagon(0.17, 0.070, 0.038, 0.).map(|p| Vec3::new(0.741, p.x - 0.12, p.y - 0.035)),
        );
        let port_inner = mesh.ring(
            octagon(0.145, 0.048, 0.026, 0.).map(|p| Vec3::new(0.742, p.x - 0.12, p.y - 0.035)),
        );
        for i in 0..8 {
            let j = (i + 1) % 8;
            mesh.face(
                &[port_outer[i], port_outer[j], port_inner[j], port_inner[i]],
                4,
            );
        }
        mesh.face(&port_inner, 2);
        mesh.mount_since(port_start, Vec3::X, 0.74);

        let aperture_start = mesh.face_count;
        let aperture =
            mesh.ring(octagon(0.025, 0.025, 0.014, 0.206).map(|p| p + Vec3::new(0.35, -0.49, 0.)));
        mesh.face(&aperture, 2);
        mesh.mount_since(aperture_start, Vec3::Z, 0.205);

        for side in [-1., 1.] {
            let channel_start = mesh.face_count;
            let mut channel = mesh.ring([
                Vec3::new(-0.50, side * 0.687, -0.155),
                Vec3::new(0.50, side * 0.687, -0.155),
                Vec3::new(0.50, side * 0.687, -0.112),
                Vec3::new(-0.50, side * 0.687, -0.112),
            ]);
            if side > 0. {
                channel.reverse();
            }
            mesh.face(&channel, 3);
            mesh.mount_since(channel_start, Vec3::Y * side, 0.686);
        }
        mesh
    }
}

fn rounded_rectangle(x: f32, y: f32, radius: f32, z: f32) -> [Vec3; 16] {
    let centers = [
        (x - radius, -y + radius),
        (x - radius, y - radius),
        (-x + radius, y - radius),
        (-x + radius, -y + radius),
    ];
    std::array::from_fn(|i| {
        let (cx, cy) = centers[i / 4];
        let angle = (i / 4) as f32 * std::f32::consts::FRAC_PI_2 - std::f32::consts::FRAC_PI_2
            + (i % 4) as f32 * std::f32::consts::FRAC_PI_6;
        Vec3::new(cx + radius * angle.cos(), cy + radius * angle.sin(), z)
    })
}

fn circle(radius: f32, z: f32) -> [Vec3; 16] {
    std::array::from_fn(|i| {
        let angle = i as f32 * std::f32::consts::TAU / 16.;
        Vec3::new(radius * angle.cos(), radius * angle.sin(), z)
    })
}

fn octagon(x: f32, y: f32, bevel: f32, z: f32) -> [Vec3; 8] {
    [
        (-x + bevel, -y),
        (x - bevel, -y),
        (x, -y + bevel),
        (x, y - bevel),
        (x - bevel, y),
        (-x + bevel, y),
        (-x, y - bevel),
        (-x, -y + bevel),
    ]
    .map(|(x, y)| Vec3::new(x, y, z))
}

#[derive(Clone, Copy, Default)]
struct VisibleFace {
    index: usize,
    depth: f32,
    color: u32,
    detail: bool,
}

pub(super) struct ModelView {
    mesh: Mesh,
    orientation: Quat,
    body_vectors: [Option<Vec3>; 2],
    azimuth: f32,
    elevation: f32,
    distance: f32,
    drag: Option<(Point<Pixels>, HitboxId)>,
    activation: Option<Subscription>,
    focus: FocusHandle,
    bounds: Option<Bounds<Pixels>>,
    projected: [Point<Pixels>; MAX_VERTICES],
    visible: [VisibleFace; MAX_FACES],
    visible_count: usize,
    shadow: [Point<Pixels>; 16],
    axes: [[Point<Pixels>; 2]; 4],
    vector_axes: [Option<[Point<Pixels>; 2]>; 2],
}

impl ModelView {
    pub(super) fn new(cx: &mut Context<Self>) -> Self {
        cx.bind_keys([
            KeyBinding::new("left", OrbitLeft, Some("ReplayModel")),
            KeyBinding::new("right", OrbitRight, Some("ReplayModel")),
            KeyBinding::new("up", OrbitUp, Some("ReplayModel")),
            KeyBinding::new("down", OrbitDown, Some("ReplayModel")),
            KeyBinding::new("=", ZoomIn, Some("ReplayModel")),
            KeyBinding::new("shift-=", ZoomIn, Some("ReplayModel")),
            KeyBinding::new("+", ZoomIn, Some("ReplayModel")),
            KeyBinding::new("-", ZoomOut, Some("ReplayModel")),
            KeyBinding::new("home", ResetCamera, Some("ReplayModel")),
        ]);
        Self {
            mesh: Mesh::housing(),
            orientation: Quat::IDENTITY,
            body_vectors: [None; 2],
            azimuth: DEFAULT_AZIMUTH,
            elevation: DEFAULT_ELEVATION,
            distance: DEFAULT_DISTANCE,
            drag: None,
            activation: None,
            bounds: None,
            focus: cx.focus_handle().tab_stop(true),
            projected: [Point::default(); MAX_VERTICES],
            visible: [VisibleFace::default(); MAX_FACES],
            visible_count: 0,
            shadow: [Point::default(); 16],
            axes: [[Point::default(); 2]; 4],
            vector_axes: [None; 2],
        }
    }

    pub(super) fn set_orientation(&mut self, q: [f32; 4], cx: &mut Context<Self>) {
        // Input is Hamilton wxyz, body -> world, with world Z up.
        let q = Quat::from_xyzw(q[1], q[2], q[3], q[0]);
        if !q.is_finite() || !q.length_squared().is_finite() || q.length_squared() < 1e-9 {
            return;
        }
        let q = q.normalize();
        if q == self.orientation || q == -self.orientation {
            return;
        }
        self.orientation = q;
        self.invalidate(cx);
    }

    pub(super) fn set_frame_vectors(
        &mut self,
        frame: Option<&ReplayFrame>,
        cx: &mut Context<Self>,
    ) {
        // Helpers return fresh, qualified body-frame observations only. Linear
        // acceleration removes gravity using 6D, before returning to body space.
        // Rendering then uses the same selected 6D/9D pose as the mesh; neither
        // vector is a world vector to which that pose should be applied twice.
        let vectors = [
            frame.and_then(magnetic_body).and_then(normalized_direction),
            frame
                .and_then(linear_acceleration_body)
                .and_then(normalized_direction),
        ];
        if self.body_vectors != vectors {
            self.body_vectors = vectors;
            self.invalidate(cx);
        }
    }

    fn invalidate(&mut self, cx: &mut Context<Self>) {
        self.bounds = None;
        cx.notify();
    }

    fn orbit(&mut self, horizontal: f32, vertical: f32, cx: &mut Context<Self>) {
        let azimuth = (self.azimuth + horizontal).rem_euclid(std::f32::consts::TAU);
        let elevation = (self.elevation + vertical).clamp(-1.25, 1.25);
        if self.azimuth != azimuth || self.elevation != elevation {
            self.azimuth = azimuth;
            self.elevation = elevation;
            self.invalidate(cx);
        }
    }

    fn zoom(&mut self, factor: f32, cx: &mut Context<Self>) {
        let distance = (self.distance * factor).clamp(2.6, 7.);
        if distance != self.distance {
            self.distance = distance;
            self.invalidate(cx);
        }
    }

    pub(super) fn cancel_interaction(&mut self, window: &mut Window) -> bool {
        let Some((_, capture)) = self.drag.take() else {
            return false;
        };
        let owned_capture = window.captured_hitbox() == Some(capture);
        if owned_capture {
            window.release_pointer();
        }
        owned_capture
    }

    fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_interaction(window);
        self.azimuth = DEFAULT_AZIMUTH;
        self.elevation = DEFAULT_ELEVATION;
        self.distance = DEFAULT_DISTANCE;
        self.invalidate(cx);
    }

    fn project(&mut self, bounds: Bounds<Pixels>) {
        if self.bounds == Some(bounds) {
            return;
        }
        self.bounds = Some(bounds);
        let eye = Vec3::new(
            self.azimuth.cos() * self.elevation.cos(),
            self.azimuth.sin() * self.elevation.cos(),
            self.elevation.sin(),
        ) * self.distance;
        let forward = -eye.normalize();
        let right = forward.cross(Vec3::Z).normalize();
        let up = right.cross(forward);
        let scale = f32::from(bounds.size.width).min(f32::from(bounds.size.height)) * 1.68;
        let project = |p: Vec3| {
            let relative = p - eye;
            // Zoom is bounded outside the entire mesh sphere; no face crosses the near plane.
            let depth = relative.dot(forward);
            point(
                bounds.center().x + px(relative.dot(right) * scale / depth),
                bounds.center().y - px(relative.dot(up) * scale / depth),
            )
        };
        for i in 0..self.mesh.vertex_count {
            self.projected[i] = project(self.orientation * self.mesh.vertices[i]);
        }
        let light = Vec3::new(-0.4, -0.6, 1.).normalize();
        self.visible_count = 0;
        for (i, face) in self.mesh.faces[..self.mesh.face_count].iter().enumerate() {
            if let Some((normal, distance)) = face.mounting_plane
                && (self.orientation * normal).dot(eye) <= distance
            {
                continue;
            }
            let center = self.orientation * face.center;
            let normal = self.orientation * face.normal;
            let toward_eye = eye - center;
            if normal.dot(toward_eye) <= 0. {
                continue;
            }
            let diffuse = normal.dot(light).max(0.);
            let half = (light + toward_eye.normalize()).normalize_or_zero();
            let highlight = normal.dot(half).max(0.).powi(24) * 0.16;
            let (base, ambient) = match face.material {
                0 => ([0.76, 0.78, 0.80], 0.68),
                1 => ([0.60, 0.63, 0.67], 0.60),
                2 => ([0.075, 0.085, 0.10], 0.75),
                3 => ([0.25, 0.28, 0.31], 0.70),
                _ => ([0.49, 0.53, 0.57], 0.72),
            };
            let channels = base.map(|v: f32| {
                ((v * (ambient + (1. - ambient) * diffuse) + highlight).clamp(0., 1.) * 255.) as u32
            });
            self.visible[self.visible_count] = VisibleFace {
                index: i,
                depth: (center - eye).dot(forward),
                detail: face.mounting_plane.is_some(),
                color: (channels[0] << 16) | (channels[1] << 8) | channels[2],
            };
            self.visible_count += 1;
        }
        // The convex shell has no overlapping front faces. Surface details must follow
        // their mounting face, not its centroid depth (which would hide off-center details).
        // Hidden mounting planes cull their details before the depth sort.
        self.visible[..self.visible_count].sort_unstable_by(|a, b| {
            a.detail
                .cmp(&b.detail)
                .then_with(|| b.depth.total_cmp(&a.depth))
        });
        for (i, p) in self.shadow.iter_mut().enumerate() {
            let angle = i as f32 * std::f32::consts::TAU / 16.;
            *p = project(Vec3::new(angle.cos() * 0.82, angle.sin() * 0.75, -1.06));
        }
        for (i, axis) in [Vec3::X, Vec3::Y, Vec3::Z, -Vec3::Z]
            .into_iter()
            .enumerate()
        {
            let axis = if i == 3 {
                axis
            } else {
                self.orientation * axis
            };
            // All overlays share the device center. Camera orbit never changes the
            // body axes' quaternion or the gravity vector's fixed world direction.
            self.axes[i] = [project(Vec3::ZERO), project(axis * DIRECTION_LENGTH)];
        }
        for (projected, vector) in self.vector_axes.iter_mut().zip(self.body_vectors) {
            *projected = vector.map(|direction| {
                [
                    project(Vec3::ZERO),
                    project(self.orientation * direction * DIRECTION_LENGTH),
                ]
            });
        }
    }

    fn paint(&mut self, bounds: Bounds<Pixels>, window: &mut Window, dark: bool) {
        self.project(bounds);
        fill_polygon(&self.shadow, rgba(0x00000012).into(), window);
        for visible in &self.visible[..self.visible_count] {
            let face = &self.mesh.faces[visible.index];
            let mut points = [Point::default(); 16];
            for (point, &index) in points.iter_mut().zip(&face.indices[..face.len]) {
                *point = self.projected[index];
            }
            fill_polygon(&points[..face.len], rgb(visible.color).into(), window);
        }
        for (index, axis) in self
            .axes
            .iter()
            .map(Some)
            .chain(self.vector_axes.iter().map(Option::as_ref))
            .enumerate()
        {
            let Some(axis) = axis else {
                continue;
            };
            let dx = f32::from(axis[1].x - axis[0].x);
            let dy = f32::from(axis[1].y - axis[0].y);
            let len = dx.hypot(dy);
            if len < 0.5 {
                continue;
            }
            let color = super::presentation::direction_color(index, dark);
            let offset = point(px(-dy / len), px(dx / len));
            fill_polygon(
                &[
                    axis[0] + offset,
                    axis[1] + offset,
                    axis[1] - offset,
                    axis[0] - offset,
                ],
                color,
                window,
            );
            let back = point(px(dx / len * 6.), px(dy / len * 6.));
            let wing = point(px(-dy / len * 3.), px(dx / len * 3.));
            fill_polygon(
                &[axis[1], axis[1] - back + wing, axis[1] - back - wing],
                color,
                window,
            );
        }
    }
}

fn fill_polygon(points: &[Point<Pixels>], color: Hsla, window: &mut Window) {
    // Native filled paths, no Lyon tessellation, CPU raster buffer or GPU readback.
    // GPUI consumes each path's vertex allocation; geometry/cache above remain fixed-size.
    let mut path = gpui::Path::new(points[0]);
    path.vertices.reserve_exact((points.len() - 2) * 3);
    for i in 1..points.len() - 1 {
        path.push_triangle(
            (points[0], points[i], points[i + 1]),
            (point(0., 1.), point(0., 1.), point(0., 1.)),
        );
    }
    window.paint_path(path, color);
}

fn register_input(owner: WeakEntity<ModelView>, hitbox: Hitbox, window: &mut Window) {
    let down = owner.clone();
    let down_hitbox = hitbox.clone();
    window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
        if phase.bubble() && event.button == MouseButton::Left && down_hitbox.is_hovered(window) {
            let _ = down.update(cx, |this, cx| {
                this.focus.focus(window, cx);
                this.drag = Some((event.position, down_hitbox.id));
            });
            window.capture_pointer(down_hitbox.id);
            cx.stop_propagation();
        }
    });
    let movement = owner.clone();
    window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
        if !phase.capture() {
            return;
        }
        let handled = movement
            .update(cx, |this, cx| {
                let Some((previous, capture)) = this.drag else {
                    return false;
                };
                if !event.dragging()
                    || !window.is_window_active()
                    || window.captured_hitbox() != Some(capture)
                {
                    this.cancel_interaction(window);
                    return false;
                }
                let dx = f32::from(event.position.x - previous.x);
                let dy = f32::from(event.position.y - previous.y);
                this.drag = Some((event.position, capture));
                if dx != 0. || dy != 0. {
                    this.orbit(-dx * 0.012, dy * 0.012, cx);
                }
                true
            })
            .unwrap_or(false);
        if handled {
            cx.stop_propagation();
        }
    });
    let up = owner.clone();
    window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
        if phase.capture()
            && event.button == MouseButton::Left
            && up
                .update(cx, |this, _| this.cancel_interaction(window))
                .unwrap_or(false)
        {
            cx.stop_propagation();
        }
    });
    let wheel = owner.clone();
    window.on_mouse_event(move |event: &ScrollEvent, phase, window, cx| {
        if phase.bubble() && hitbox.is_hovered(window) {
            let delta = f32::from(event.delta.pixel_delta(px(24.)).y);
            if delta.is_finite() {
                let _ = wheel.update(cx, |this, cx| {
                    this.zoom((-delta * 0.003).exp(), cx);
                });
            }
            cx.stop_propagation();
        }
    });
}

impl Render for ModelView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.activation.is_none() {
            self.activation = Some(cx.observe_window_activation(window, |this, window, _| {
                if !window.is_window_active() {
                    this.cancel_interaction(window);
                }
            }));
        }
        let owner = cx.weak_entity();
        div()
            .w_full()
            .min_w_0()
            .h(px(220.))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" && this.cancel_interaction(window) {
                    cx.stop_propagation();
                }
            }))
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex_shrink_0()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(tr("replay.model.title")),
                    )
                    .child(
                        Button::new("reset-model-view")
                            .label(tr("replay.model.reset_view"))
                            .small()
                            .on_click(cx.listener(|this, _, window, cx| this.reset(window, cx))),
                    ),
            )
            .child(
                div()
                    .id("replay-model-viewport")
                    .role(gpui::Role::Group)
                    .aria_label(tr("replay.model.title"))
                    .aria_description(tr("replay.model.interaction_hint"))
                    .track_focus(&self.focus)
                    .key_context("ReplayModel")
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded_md()
                    .focus_visible(|style| style.border_color(cx.theme().ring))
                    .on_action(cx.listener(|this, _: &OrbitLeft, _, cx| {
                        this.orbit(KEYBOARD_ORBIT_STEP, 0., cx);
                    }))
                    .on_action(cx.listener(|this, _: &OrbitRight, _, cx| {
                        this.orbit(-KEYBOARD_ORBIT_STEP, 0., cx);
                    }))
                    .on_action(cx.listener(|this, _: &OrbitUp, _, cx| {
                        this.orbit(0., -KEYBOARD_ORBIT_STEP, cx);
                    }))
                    .on_action(cx.listener(|this, _: &OrbitDown, _, cx| {
                        this.orbit(0., KEYBOARD_ORBIT_STEP, cx);
                    }))
                    .on_action(cx.listener(|this, _: &ZoomIn, _, cx| {
                        this.zoom(KEYBOARD_ZOOM_FACTOR, cx);
                    }))
                    .on_action(cx.listener(|this, _: &ZoomOut, _, cx| {
                        this.zoom(KEYBOARD_ZOOM_FACTOR.recip(), cx);
                    }))
                    .on_action(cx.listener(|this, _: &ResetCamera, window, cx| {
                        this.reset(window, cx);
                    }))
                    .w_full()
                    .min_w_0()
                    .min_h(px(120.))
                    .flex_1()
                    .child(
                        canvas(
                            |bounds, window, _| {
                                window.insert_hitbox(bounds, HitboxBehavior::Normal)
                            },
                            move |bounds, hitbox, window, cx| {
                                register_input(owner.clone(), hitbox, window);
                                let dark = cx.theme().is_dark();
                                let _ =
                                    owner.update(cx, |this, _| this.paint(bounds, window, dark));
                            },
                        )
                        .size_full(),
                    ),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .whitespace_normal()
                    .child(tr("replay.model.interaction_hint")),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::normalized_direction;
    use glam::Vec3;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{
        AppContext as _, Context, Entity, FocusHandle, IntoElement, Render, TestAppContext, Window,
        div, prelude::*,
    };

    struct ModelHarness {
        model: Entity<super::ModelView>,
        focus: FocusHandle,
    }

    impl Render for ModelHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .track_focus(&self.focus)
                .tab_group()
                .child(self.model.clone())
        }
    }

    #[gpui_kit::test]
    fn camera_keyboard_is_tabbable_scoped_and_preserves_recorded_pose(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (handle, harness) = cx.update(|cx| {
            gpui_kit::open_window(Default::default(), cx, |window, cx| {
                cx.new(|cx| {
                    // A standalone page still needs a non-tab owner for Root's key context.
                    let focus = cx.focus_handle().tab_stop(false);
                    focus.focus(window, cx);
                    ModelHarness {
                        model: cx.new(super::ModelView::new),
                        focus,
                    }
                })
            })
            .unwrap()
        });
        let model = cx.update(|cx| harness.read(cx).model.clone());
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(harness.read(cx).focus.is_focused(window));
            window.press("tab", cx);
            assert_eq!(window.find("reset-model-view").focused(), Some(true));
            window.press("right", cx);
            assert_eq!(model.read(cx).azimuth, super::DEFAULT_AZIMUTH);
            window.press("tab", cx);
            assert!(model.read(cx).focus.is_focused(window));
            let orientation = model.read(cx).orientation;
            window.press("right", cx);
            assert_ne!(model.read(cx).azimuth, super::DEFAULT_AZIMUTH);
            window.press("up", cx);
            assert_ne!(model.read(cx).elevation, super::DEFAULT_ELEVATION);
            window.press("=", cx);
            assert!(model.read(cx).distance < super::DEFAULT_DISTANCE);
            window.press("-", cx);
            assert!((model.read(cx).distance - super::DEFAULT_DISTANCE).abs() < 1e-5);
            window.press("+", cx);
            assert!(model.read(cx).distance < super::DEFAULT_DISTANCE);
            assert_eq!(model.read(cx).orientation, orientation);
            window.press("home", cx);
            assert_eq!(model.read(cx).azimuth, super::DEFAULT_AZIMUTH);
            assert_eq!(model.read(cx).elevation, super::DEFAULT_ELEVATION);
            assert_eq!(model.read(cx).distance, super::DEFAULT_DISTANCE);
            window.press("shift-tab", cx);
            assert!(!model.read(cx).focus.is_focused(window));
            window.press("right", cx);
            assert_eq!(model.read(cx).azimuth, super::DEFAULT_AZIMUTH);
        })
        .unwrap();
    }

    #[test]
    fn direction_hides_zero_and_nonfinite_observations() {
        assert_eq!(normalized_direction([0.; 3]), None);
        assert_eq!(normalized_direction([-0.; 3]), None);
        for invalid in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(normalized_direction([1., invalid, 0.]), None);
        }
    }

    #[test]
    fn direction_is_bounded_independently_of_sensor_magnitude() {
        for scale in [f32::MIN_POSITIVE, 1., f32::MAX / 4.] {
            let direction = normalized_direction([scale, -2. * scale, 3. * scale]).unwrap();
            let expected = Vec3::new(1., -2., 3.).normalize();
            assert!((direction - expected).length() < 1e-6);
            assert!((direction.length() - 1.).abs() < 1e-6);
        }
        assert_eq!(
            normalized_direction([f32::from_bits(1), 0., 0.]),
            Some(Vec3::X)
        );
        assert_eq!(normalized_direction([0., -f32::MAX, 0.]), Some(Vec3::NEG_Y));
    }
}
