use std::collections::HashMap;

use base64::engine::general_purpose::URL_SAFE_NO_PAD as base64_url;
use base64::Engine;
use rand::{thread_rng, RngCore};
use serde::Serialize;

use crate::config::Config;
use crate::template::Template;

pub const URL_GET_COMPANY: &str = "https://corplink.volcengine.cn/api/match";

macro_rules! api_url {
    ($path:expr) => {
        concat!("{{url}}", $path, "?", "app_version={{app_version}}&brand={{brand}}&build_number={{build_number}}&client_source={{client_source}}&language={{language}}&model={{model}}&os={{os}}&os_version={{version}}&timestamp={{timestamp}}")
    };
}

const URL_GET_LOGIN_METHOD: &str = api_url!("/api/login/setting");
const URL_GET_TPS_LOGIN_METHOD: &str =
    "{{url}}/api/tpslogin/link?app_version={{app_version}}&brand={{brand}}&build_number={{build_number}}&client_source={{client_source}}&code_challenge={{code_challenge}}&language={{language}}&model={{model}}&os={{os}}&os_version={{version}}&timestamp={{timestamp}}";
const URL_GET_TPS_TOKEN_CHECK: &str =
    "{{url}}/api/tpslogin/token/check?app_version={{app_version}}&brand={{brand}}&build_number={{build_number}}&client_source={{client_source}}&code_challenge={{code_challenge}}&language={{language}}&model={{model}}&os={{os}}&os_version={{version}}&timestamp={{timestamp}}";
const URL_REQUEST_CODE: &str = api_url!("/api/login/code/send");
const URL_VERIFY_CODE: &str = api_url!("/api/login/code/verify");
const URL_LOGIN_PASSWORD: &str = api_url!("/api/v1/login");
const URL_LOGIN_MFA_SEND: &str = api_url!("/api/v1/login/mfa/send");
const URL_LOGIN_MFA_VERIFY: &str = api_url!("/api/v1/login/mfa/verify");
const URL_LIST_VPN: &str = api_url!("/api/vpn/list");

const URL_PING_VPN_HOST: &str =
    "{{url}}/vpn/ping?app_version={{app_version}}&brand={{brand}}&build_number={{build_number}}&client_source={{client_source}}&language={{language}}&model={{model}}&os={{os}}&os_version={{version}}&timestamp={{timestamp}}";
const URL_FETCH_PEER_INFO: &str =
    "{{url}}/vpn/conn?app_version={{app_version}}&brand={{brand}}&build_number={{build_number}}&client_source={{client_source}}&language={{language}}&model={{model}}&os={{os}}&os_version={{version}}&timestamp={{timestamp}}";
const URL_OPERATE_VPN: &str =
    "{{url}}/vpn/report?app_version={{app_version}}&brand={{brand}}&build_number={{build_number}}&client_source={{client_source}}&language={{language}}&model={{model}}&os={{os}}&os_version={{version}}&timestamp={{timestamp}}";
const URL_OTP: &str = api_url!("/api/v2/p/otp");

#[derive(Clone, Hash, Eq, PartialEq, Debug)]
pub enum ApiName {
    LoginMethod,
    TpsLoginMethod,
    TpsTokenCheck,
    RequestEmailCode,
    LoginPassword,
    LoginEmail,
    LoginMfaSend,
    LoginMfaVerify,
    ListVPN,

    PingVPN,
    ConnectVPN,
    KeepAliveVPN,
    DisconnectVPN,
    OTP,
}

#[derive(Clone, Serialize)]
struct UserUrlParam {
    url: String,
    os: String,
    version: String,
    app_version: String,
    brand: String,
    build_number: String,
    client_source: String,
    language: String,
    model: String,
    code_challenge: String,
    timestamp: String,
}

#[derive(Clone, Serialize)]
pub struct VpnUrlParam {
    pub url: String,
    os: String,
    version: String,
    app_version: String,
    brand: String,
    build_number: String,
    client_source: String,
    language: String,
    model: String,
    timestamp: String,
}

#[derive(Clone)]
pub struct ApiUrl {
    user_param: UserUrlParam,
    pub vpn_param: VpnUrlParam,
    api_template: HashMap<ApiName, Template>,
    code_challenge: String,
}

impl ApiUrl {
    pub fn new(conf: &Config) -> ApiUrl {
        let os = "iOS".to_string();
        let version = "15.8.3".to_string();
        let app_version = "3.2.16".to_string();
        let brand = "Apple".to_string();
        let build_number = "551".to_string();
        let client_source = "FeiLian".to_string();
        let language = "en".to_string();
        let model = "iPhone9,1".to_string();
        let code_challenge = Self::generate_code_challenge();
        let timestamp = Self::current_timestamp();
        let mut api_template = HashMap::new();

        api_template.insert(ApiName::LoginMethod, Template::new(URL_GET_LOGIN_METHOD));
        api_template.insert(
            ApiName::TpsLoginMethod,
            Template::new(URL_GET_TPS_LOGIN_METHOD),
        );
        api_template.insert(
            ApiName::TpsTokenCheck,
            Template::new(URL_GET_TPS_TOKEN_CHECK),
        );
        api_template.insert(ApiName::RequestEmailCode, Template::new(URL_REQUEST_CODE));
        api_template.insert(ApiName::LoginEmail, Template::new(URL_VERIFY_CODE));
        api_template.insert(ApiName::LoginPassword, Template::new(URL_LOGIN_PASSWORD));
        api_template.insert(ApiName::LoginMfaSend, Template::new(URL_LOGIN_MFA_SEND));
        api_template.insert(ApiName::LoginMfaVerify, Template::new(URL_LOGIN_MFA_VERIFY));
        api_template.insert(ApiName::ListVPN, Template::new(URL_LIST_VPN));
        api_template.insert(ApiName::PingVPN, Template::new(URL_PING_VPN_HOST));
        api_template.insert(ApiName::ConnectVPN, Template::new(URL_FETCH_PEER_INFO));
        api_template.insert(ApiName::KeepAliveVPN, Template::new(URL_OPERATE_VPN));
        api_template.insert(ApiName::DisconnectVPN, Template::new(URL_OPERATE_VPN));
        api_template.insert(ApiName::OTP, Template::new(URL_OTP));

        ApiUrl {
            user_param: UserUrlParam {
                url: conf.server.clone().unwrap(),
                os: os.clone(),
                version: version.clone(),
                app_version: app_version.clone(),
                brand: brand.clone(),
                build_number: build_number.clone(),
                client_source: client_source.clone(),
                language: language.clone(),
                model: model.clone(),
                code_challenge: code_challenge.clone(),
                timestamp: timestamp.clone(),
            },
            vpn_param: VpnUrlParam {
                url: "".to_string(),
                os,
                version,
                app_version,
                brand,
                build_number,
                client_source,
                language,
                model,
                timestamp,
            },
            api_template,
            code_challenge,
        }
    }

    pub fn get_api_url(&mut self, name: &ApiName) -> String {
        // refresh timestamp for each request
        let ts = Self::current_timestamp();
        self.user_param.timestamp = ts.clone();
        self.vpn_param.timestamp = ts;

        let user_param = &self.user_param;
        let vpn_param = &self.vpn_param;
        match name {
            ApiName::LoginMethod => self.api_template[name].render(user_param),
            ApiName::TpsLoginMethod => self.api_template[name].render(user_param),
            ApiName::TpsTokenCheck => self.api_template[name].render(user_param),
            ApiName::RequestEmailCode => self.api_template[name].render(user_param),
            ApiName::LoginEmail => self.api_template[name].render(user_param),
            ApiName::LoginPassword => self.api_template[name].render(user_param),
            ApiName::LoginMfaSend => self.api_template[name].render(user_param),
            ApiName::LoginMfaVerify => self.api_template[name].render(user_param),
            ApiName::ListVPN => self.api_template[name].render(user_param),
            ApiName::OTP => self.api_template[name].render(user_param),

            ApiName::PingVPN => self.api_template[name].render(vpn_param),
            ApiName::ConnectVPN => self.api_template[name].render(vpn_param),
            ApiName::KeepAliveVPN => self.api_template[name].render(vpn_param),
            ApiName::DisconnectVPN => self.api_template[name].render(vpn_param),
        }
    }

    pub fn refresh_code_challenge(&mut self) {
        let challenge = Self::generate_code_challenge();
        self.user_param.code_challenge = challenge.clone();
        self.code_challenge = challenge;
    }

    fn generate_code_challenge() -> String {
        let mut rng = thread_rng();
        let mut bytes = [0u8; 32];
        rng.fill_bytes(&mut bytes);
        base64_url.encode(bytes)
    }

    fn current_timestamp() -> String {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .to_string()
    }
}
