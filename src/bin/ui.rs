slint::slint! {
    import { AboutSlint, Button, VerticalBox } from "std-widgets.slint";

    import { Button, GroupBox, SpinBox, ComboBox, CheckBox, LineEdit, TabWidget, VerticalBox, HorizontalBox,
        Slider, ProgressIndicator, SpinBox, Switch, Spinner, GridBox, StandardButton, TextEdit, ScrollView} from "std-widgets.slint";


    export component MainWindow inherits Window {
        width: 1280px;
        height: 720px;
        title: "quick-assistant";

        callback handle_message;
        callback disable_send_if_empty_message;
        in-out property <string> message_history <=> message_history_text.text;
        in-out property <string> message <=> message_lineedit.text;
        in property <bool> send_button_enabled <=> send_button.enabled;

        VerticalLayout {
            width: 66%;
            padding-bottom: 100px;
            // TextEdit {

            // }
            Rectangle {
                border-color: darkslategrey;
                // border-width: 1px;
                border-radius: 10px;
                // drop-shadow-color: blue;
                // ScrollView {
                    HorizontalLayout {
                        padding: 10px;
                        alignment: start;
                        VerticalLayout {
                            alignment: end;
                            message_history_text := Text {
                                wrap: word-wrap;
                            }
                        }
                    }

                // }
            }
            Rectangle {
                border-color: darkslategray;
                border-width: 1px;
                border-radius: 15px;
                // drop-shadow-color: blue;
                height: 50px;
                // width: 100px;
                // height: 100px;

                HorizontalBox {
                    message_lineedit := LineEdit {
                        accepted => {
                            handle_message();
                        }
                        // width: 80%;
                        edited => {disable_send_if_empty_message()}
                    }
                    send_button := Button {
                        text: "Send";
                        // width: 50px;
                        primary: true;
                        clicked => {
                            handle_message();
                        }
                        enabled: false;
                    }
                }
            }
        }
    }
}
fn main() {
    println!("Hello World!");

    // let config = slint_build::CompilerConfiguration::new().with_style("hello".to_string());
    // slint_build::compile_with_config();

    let main_window = MainWindow::new().unwrap();

    let main_window_weak = main_window.as_weak();
    main_window.on_handle_message(move || {
        let main_window: MainWindow = main_window_weak.unwrap();

        let message: String = main_window.get_message().into();

        if message.trim().is_empty() {
            return;
        }

        let mut current_message_history: String = main_window.get_message_history().into();
        current_message_history.push_str(&format!("\n{}", message));

        main_window.set_message_history(current_message_history.into());
        main_window.set_message("".into());
        main_window.set_send_button_enabled(false);
    });

    let main_window_weak = main_window.as_weak();
    main_window.on_disable_send_if_empty_message(move || {
        let main_window: MainWindow = main_window_weak.unwrap();
        let message: String = main_window.get_message().into();
        main_window.set_send_button_enabled(!message.trim().is_empty());
    });

    main_window.run().unwrap();
}
