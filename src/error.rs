minimer::define_app_error!(pub AppError);

minimer::impl_from_error!(AppError, secr::error::Error);
minimer::impl_from_error!(AppError, serde_json::Error);

// No From<ureq::Error>: its text can include the request URL, which holds the bot token.
