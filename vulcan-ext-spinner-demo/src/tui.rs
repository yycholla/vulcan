use std::sync::Arc;

use vulcan_frontend_api::{
    FrontendCodeExtension, FrontendCtx, FrontendExtensionRegistration, WidgetContent,
};

pub struct SpinnerDemoFrontendExtension;

impl FrontendCodeExtension for SpinnerDemoFrontendExtension {
    fn id(&self) -> &'static str {
        "spinner-demo"
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    fn frontend_capabilities(&self) -> Vec<&'static str> {
        vec!["text_io", "status_widgets"]
    }

    fn on_event(&self, payload: &serde_json::Value, ctx: &mut FrontendCtx) {
        let widget_id = payload
            .get("widget_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("spinner-demo");
        match payload.get("kind").and_then(serde_json::Value::as_str) {
            Some("spinner") => {
                let label = payload
                    .get("label")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("working");
                ctx.ui
                    .set_widget(widget_id, Some(WidgetContent::Spinner(label.into())));
            }
            Some("progress") => {
                let label = payload
                    .get("label")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("working");
                let ratio = payload
                    .get("ratio")
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(0.0);
                ctx.ui
                    .set_widget(widget_id, Some(WidgetContent::progress(label, ratio)));
            }
            Some("text") => {
                let text = payload
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                ctx.ui
                    .set_widget(widget_id, Some(WidgetContent::Text(text.into())));
            }
            Some("clear") => ctx.ui.set_widget(widget_id, None),
            _ => {}
        }
    }
}

inventory::submit! {
    FrontendExtensionRegistration {
        register: || Arc::new(SpinnerDemoFrontendExtension) as Arc<dyn FrontendCodeExtension>,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spinner_frontend_sets_and_clears_widget() {
        let ext = SpinnerDemoFrontendExtension;
        let mut ctx = FrontendCtx::default().with_extension("spinner-demo");

        ext.on_event(
            &serde_json::json!({
                "widget_id": "job",
                "kind": "spinner",
                "label": "working"
            }),
            &mut ctx,
        );
        ext.on_event(
            &serde_json::json!({
                "widget_id": "job",
                "kind": "clear"
            }),
            &mut ctx,
        );

        let updates = ctx.ui.drain_widget_updates();
        assert_eq!(updates.len(), 2);
        assert_eq!(
            updates[0].content,
            Some(WidgetContent::Spinner("working".into()))
        );
        assert_eq!(updates[1].content, None);
    }
}
