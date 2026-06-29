use crate::analyze::Analysis;

pub struct Document {
    #[allow(dead_code)]
    pub text: String,
    pub analysis: Option<Analysis>,
}
