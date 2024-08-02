pub struct Blob {
    id: Option<String>,
    data: String,
}
impl Blob {
    pub fn new(data: String) -> Self {
        Blob { id: None, data }
    }

    pub fn get_type(&self) -> &str {
        "blob"
    }

    pub fn set_id(&mut self, id: String) {
        self.id = Some(id);
    }

    pub fn to_string(&self) -> &str {
        &self.data
    }
}