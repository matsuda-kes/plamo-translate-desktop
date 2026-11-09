//! Inference device and context settings.

pub use appdata::devices::{
    candidates, choose, device_label, first_measurement, model_options, resolve, session_options,
};
use engine::llama::{Device, DeviceKind};
use gpui_kit::component::button::Button;
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IndexPath, Sizable as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rust_i18n::t;

use crate::lang::{self, Choice};
use crate::models::format_bytes;
use crate::settings::{BackendSettings, DeviceSetting, MeasuredChoice};
use crate::view::scaled;

/// Memory available to the model on `device`, or system memory for the CPU.
pub fn available_memory(device: Option<&Device>, devices: &[Device]) -> u64 {
    match device {
        Some(d) => d.memory_free,
        None => devices
            .iter()
            .find(|d| d.kind == DeviceKind::Cpu)
            .map_or(0, |d| d.memory_free),
    }
}

/// Rough memory needed to run a model file at the full 32k context: weights, the KV cache and
/// compute buffers. With windowed caches for the sliding-window layers, the KV cache is 5-17%
/// of the file size across the published models (measured 297 MiB for 2B Q4_K_M).
pub fn estimate_required(model_bytes: u64) -> u64 {
    model_bytes + model_bytes / 5 + (384 << 20)
}

pub enum BackendEvent {
    /// The device setting changed.
    Changed(DeviceSetting),
    /// Measure the devices with the active model and choose the fastest automatically.
    Measure,
}

type DeviceSelect = SelectState<Vec<Choice<DeviceSetting>>>;

pub struct BackendPanel {
    settings: BackendSettings,
    devices: Vec<Device>,
    /// Size of the active model file, for the memory check. `None` without a model, which also
    /// means nothing to measure with.
    model_bytes: Option<u64>,
    /// A measurement is running.
    measuring: bool,
    device_select: Entity<DeviceSelect>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<BackendEvent> for BackendPanel {}

impl BackendPanel {
    pub fn new(settings: BackendSettings, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let device_choices = device_choices(&[], None);
        let selected = lang::position(&device_choices, &settings.device).map(IndexPath::new);
        let device_select = cx.new(|cx| {
            SelectState::new(
                device_choices,
                selected.or(Some(IndexPath::new(0))),
                window,
                cx,
            )
        });

        let subscriptions =
            vec![
                cx.subscribe_in(&device_select, window, |this, _, event, _, cx| {
                    if let SelectEvent::Confirm(Some(device)) = event
                        && *device != this.settings.device
                    {
                        this.settings.device = device.clone();
                        cx.emit(BackendEvent::Changed(device.clone()));
                        cx.notify();
                    }
                }),
            ];

        Self {
            settings,
            devices: Vec::new(),
            model_bytes: None,
            measuring: false,
            device_select,
            _subscriptions: subscriptions,
        }
    }

    pub fn set_devices(
        &mut self,
        devices: Vec<Device>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let choices = device_choices(&devices, self.settings.measured.as_ref());
        let current = self.settings.device.clone();
        let known = lang::position(&choices, &current).is_some();
        self.devices = devices;
        self.device_select.update(cx, |state, cx| {
            state.set_items(choices, window, cx);
            state.set_selected_value(
                if known {
                    &current
                } else {
                    &DeviceSetting::Auto
                },
                window,
                cx,
            );
        });
        cx.notify();
    }

    /// Rebuilds the option labels after the UI language changed.
    pub fn refresh_locale(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let devices = std::mem::take(&mut self.devices);
        self.set_devices(devices, window, cx);
    }

    pub fn set_model_bytes(&mut self, bytes: Option<u64>, cx: &mut Context<Self>) {
        self.model_bytes = bytes;
        cx.notify();
    }

    /// Takes settings changed elsewhere, such as by a measurement.
    pub fn set_settings(
        &mut self,
        settings: BackendSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings = settings;
        let devices = std::mem::take(&mut self.devices);
        self.set_devices(devices, window, cx);
    }

    pub fn set_measuring(&mut self, measuring: bool, cx: &mut Context<Self>) {
        self.measuring = measuring;
        cx.notify();
    }
}

fn device_choices(
    devices: &[Device],
    measured: Option<&MeasuredChoice>,
) -> Vec<Choice<DeviceSetting>> {
    let auto_target = resolve(&DeviceSetting::Auto, measured, devices);
    let mut choices = vec![
        Choice::new(
            DeviceSetting::Auto,
            t!("backend.auto", device = device_label(auto_target)).into_owned(),
            &[],
        ),
        Choice::new(DeviceSetting::Cpu, t!("backend.cpu").into_owned(), &[]),
    ];
    choices.extend(
        devices
            .iter()
            .filter(|d| d.kind != DeviceKind::Cpu)
            .map(|d| {
                Choice::new(
                    DeviceSetting::Device(d.name.clone()),
                    t!(
                        "backend.device_choice",
                        name = d.name,
                        description = d.description,
                        free = format_bytes(d.memory_free)
                    )
                    .into_owned(),
                    &[],
                )
            }),
    );
    choices
}

impl Render for BackendPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let warning = cx.theme().warning;
        let muted = cx.theme().muted_foreground;
        let measured = self.settings.measured.as_ref();
        let device = resolve(&self.settings.device, measured, &self.devices);
        // The last measurement, while it holds for these devices.
        let results = measured
            .filter(|m| {
                !self.devices.is_empty() && m.hardware == appdata::devices::hardware(&self.devices)
            })
            .map(|m| {
                let results = m
                    .results
                    .iter()
                    .map(|r| match r.tokens_per_second {
                        Some(speed) => t!(
                            "backend.measured_speed",
                            name = r.name,
                            speed = format!("{speed:.0}")
                        )
                        .into_owned(),
                        None => t!("backend.measured_failed", name = r.name).into_owned(),
                    })
                    .collect::<Vec<_>>()
                    .join(" · ");
                t!("backend.measured", results = results).into_owned()
            });
        let available = available_memory(device, &self.devices);
        let memory_warning = self.model_bytes.and_then(|bytes| {
            let required = estimate_required(bytes);
            (!self.devices.is_empty() && required > available).then(|| {
                t!(
                    "backend.memory_warning",
                    device = device_label(device),
                    required = format_bytes(required),
                    available = format_bytes(available)
                )
                .into_owned()
            })
        });

        v_flex()
            .gap_2()
            .text_sm()
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        div()
                            .w(scaled(110.))
                            .child(t!("backend.device").into_owned()),
                    )
                    .child(
                        Select::new(&self.device_select)
                            .w(scaled(420.))
                            .menu_width(scaled(480.)),
                    )
                    .child(
                        Button::new("measure-devices")
                            .small()
                            .label(if self.measuring {
                                t!("backend.measuring").into_owned()
                            } else {
                                t!("backend.measure").into_owned()
                            })
                            .tooltip(t!("backend.measure_tooltip").into_owned())
                            .disabled(
                                self.measuring
                                    || self.model_bytes.is_none()
                                    || self.devices.is_empty(),
                            )
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(BackendEvent::Measure))),
                    ),
            )
            .when_some(results, |this, text| {
                this.child(div().text_xs().text_color(muted).child(text))
            })
            .when_some(memory_warning, |this, text| {
                this.child(div().text_xs().text_color(warning).child(text))
            })
    }
}

#[cfg(test)]
mod tests {
    use engine::llama::{Device, DeviceKind};

    use super::{DeviceSetting, estimate_required, resolve};

    fn resolve_unmeasured<'a>(
        setting: &DeviceSetting,
        devices: &'a [Device],
    ) -> Option<&'a Device> {
        resolve(setting, None, devices)
    }

    fn device(index: usize, name: &str, kind: DeviceKind, free_gb: u64) -> Device {
        Device {
            index,
            name: name.into(),
            description: String::new(),
            backend: String::new(),
            memory_total: free_gb << 30,
            memory_free: free_gb << 30,
            kind,
        }
    }

    #[test]
    fn auto_prefers_the_largest_discrete_gpu() {
        let devices = [
            device(0, "CPU", DeviceKind::Cpu, 64),
            device(1, "Vulkan0", DeviceKind::Gpu, 16),
            device(2, "Vulkan1", DeviceKind::IntegratedGpu, 32),
        ];
        assert_eq!(
            resolve_unmeasured(&DeviceSetting::Auto, &devices)
                .unwrap()
                .name,
            "Vulkan0"
        );
        assert!(resolve_unmeasured(&DeviceSetting::Cpu, &devices).is_none());
        let igpu = DeviceSetting::Device("Vulkan1".into());
        assert_eq!(resolve_unmeasured(&igpu, &devices).unwrap().name, "Vulkan1");
        let gone = DeviceSetting::Device("CUDA0".into());
        assert_eq!(resolve_unmeasured(&gone, &devices).unwrap().name, "Vulkan0");
        // Without a discrete GPU (and unmeasured), automatic means the CPU.
        assert!(resolve_unmeasured(&DeviceSetting::Auto, &devices[..1]).is_none());
    }

    #[test]
    fn estimate_covers_weights_and_buffers() {
        let gib = 1u64 << 30;
        assert!(estimate_required(gib) > gib + gib / 5);
        assert!(estimate_required(2 * gib) > estimate_required(gib));
    }
}
