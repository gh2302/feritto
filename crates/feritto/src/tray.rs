use crossbeam_channel::Sender;
use feritto_ui::Control;
struct Tray {
    controls: Sender<Control>,
}
impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "feritto".into()
    }
    fn title(&self) -> String {
        "feritto clipboard history".into()
    }
    fn icon_name(&self) -> String {
        "edit-paste".into()
    }
    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.controls.send(Control::Show);
    }
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::StandardItem;
        [
            ("Show history", Control::Show),
            ("Pause / resume capture", Control::Pause),
            ("Settings", Control::Settings),
            ("Quit", Control::Quit),
        ]
        .into_iter()
        .map(|(label, control)| {
            StandardItem {
                label: label.into(),
                activate: Box::new(move |tray: &mut Self| {
                    let _ = tray.controls.send(control.clone());
                }),
                ..Default::default()
            }
            .into()
        })
        .collect()
    }
}
pub fn start(controls: Sender<Control>) {
    std::thread::spawn(move || {
        use ksni::blocking::TrayMethods;
        match (Tray { controls }).spawn() {
            Ok(_handle) => loop {
                std::thread::park();
            },
            Err(error) => {
                tracing::info!(%error,"tray unavailable; use Ctrl+grave or feritto --toggle")
            }
        }
    });
}
