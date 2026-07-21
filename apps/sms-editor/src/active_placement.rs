use sms_authoring::AssetId;

/// The one placement source currently armed for viewport click placement.
///
/// Keeping object templates and project models in one enum prevents a stale
/// payload from silently winning after the user chooses another content item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ActivePlacement {
    Object { factory_name: String },
    Model { asset_id: AssetId },
}

impl ActivePlacement {
    pub(super) fn object_factory(&self) -> Option<&str> {
        match self {
            Self::Object { factory_name } => Some(factory_name),
            Self::Model { .. } => None,
        }
    }

    pub(super) fn model_asset(&self) -> Option<AssetId> {
        match self {
            Self::Object { .. } => None,
            Self::Model { asset_id } => Some(*asset_id),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_placement_exposes_only_its_selected_payload() {
        let object = ActivePlacement::Object {
            factory_name: "Coin".to_string(),
        };
        assert_eq!(object.object_factory(), Some("Coin"));
        assert_eq!(object.model_asset(), None);

        let id = AssetId::new();
        let model = ActivePlacement::Model { asset_id: id };
        assert_eq!(model.object_factory(), None);
        assert_eq!(model.model_asset(), Some(id));
    }
}
