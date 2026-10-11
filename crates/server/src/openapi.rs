//! OpenAPI document served at `/api/openapi.json` and rendered at `/api/docs`.

use utoipa::openapi::path::HttpMethod;
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::openapi::{OpenApi, Ref};
use utoipa::{Modify, OpenApi as OpenApiDerive};

use crate::routes::account::{
    AuditView, CreateToken, CreateUser, CreatedToken, LoginRequest, PasswordChange,
    SessionResponse, SetupRequest, SetupStatus, TokenView, TotpDisable, TotpEnable,
    TotpSetupResponse, UpdateUser, UserView,
};
use crate::routes::health::HealthResponse;

#[derive(OpenApiDerive)]
#[openapi(
    info(
        title = "Rudder API",
        version = env!("CARGO_PKG_VERSION"),
        description = "Control-plane API for the Rudder deployment panel."
    ),
    components(schemas(
        HealthResponse,
        SetupStatus,
        SetupRequest,
        LoginRequest,
        SessionResponse,
        UserView,
        PasswordChange,
        TotpSetupResponse,
        TotpEnable,
        TotpDisable,
        CreateUser,
        UpdateUser,
        CreateToken,
        TokenView,
        CreatedToken,
        AuditView
    )),
    modifiers(&SecurityAddon),
    tags(
        (name = "system", description = "Process health"),
        (name = "auth", description = "Setup, sessions, users, tokens, and audit")
    )
)]
struct ApiDoc;

struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut OpenApi) {
        let Some(components) = openapi.components.as_mut() else {
            return;
        };
        components.add_security_scheme(
            "bearer",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .bearer_format("API token")
                    .description(Some(
                        "Scoped API token. Send `Authorization: Bearer rdr_...`.",
                    ))
                    .build(),
            ),
        );
    }
}

struct Route {
    path: &'static str,
    method: HttpMethod,
    tag: &'static str,
    summary: &'static str,
    request: Option<&'static str>,
    response: Option<&'static str>,
    status: u16,
}

/// Full document, including paths added outside the derive macro.
pub fn document() -> OpenApi {
    let mut doc = ApiDoc::openapi();
    for route in ROUTES {
        add(&mut doc, route);
    }
    doc
}

const ROUTES: &[Route] = &[
    Route {
        path: "/api/health",
        method: HttpMethod::Get,
        tag: "system",
        summary: "Health",
        request: None,
        response: Some("HealthResponse"),
        status: 200,
    },
    Route {
        path: "/api/v1/setup/status",
        method: HttpMethod::Get,
        tag: "auth",
        summary: "Setup status",
        request: None,
        response: Some("SetupStatus"),
        status: 200,
    },
    Route {
        path: "/api/v1/setup",
        method: HttpMethod::Post,
        tag: "auth",
        summary: "Create the owner",
        request: Some("SetupRequest"),
        response: Some("SessionResponse"),
        status: 200,
    },
    Route {
        path: "/api/v1/login",
        method: HttpMethod::Post,
        tag: "auth",
        summary: "Sign in",
        request: Some("LoginRequest"),
        response: Some("SessionResponse"),
        status: 200,
    },
    Route {
        path: "/api/v1/logout",
        method: HttpMethod::Post,
        tag: "auth",
        summary: "Sign out",
        request: None,
        response: None,
        status: 204,
    },
    Route {
        path: "/api/v1/me",
        method: HttpMethod::Get,
        tag: "auth",
        summary: "Current session",
        request: None,
        response: Some("SessionResponse"),
        status: 200,
    },
    Route {
        path: "/api/v1/me/password",
        method: HttpMethod::Post,
        tag: "auth",
        summary: "Change password",
        request: Some("PasswordChange"),
        response: None,
        status: 204,
    },
    Route {
        path: "/api/v1/me/totp/setup",
        method: HttpMethod::Post,
        tag: "auth",
        summary: "Start authenticator setup",
        request: None,
        response: Some("TotpSetupResponse"),
        status: 200,
    },
    Route {
        path: "/api/v1/me/totp/enable",
        method: HttpMethod::Post,
        tag: "auth",
        summary: "Enable authenticator",
        request: Some("TotpEnable"),
        response: None,
        status: 204,
    },
    Route {
        path: "/api/v1/me/totp/disable",
        method: HttpMethod::Post,
        tag: "auth",
        summary: "Disable authenticator",
        request: Some("TotpDisable"),
        response: None,
        status: 204,
    },
    Route {
        path: "/api/v1/users",
        method: HttpMethod::Get,
        tag: "auth",
        summary: "List users",
        request: None,
        response: Some("UserView"),
        status: 200,
    },
    Route {
        path: "/api/v1/users",
        method: HttpMethod::Post,
        tag: "auth",
        summary: "Create user",
        request: Some("CreateUser"),
        response: Some("UserView"),
        status: 201,
    },
    Route {
        path: "/api/v1/users/{id}",
        method: HttpMethod::Patch,
        tag: "auth",
        summary: "Change a user's role",
        request: Some("UpdateUser"),
        response: Some("UserView"),
        status: 200,
    },
    Route {
        path: "/api/v1/users/{id}",
        method: HttpMethod::Delete,
        tag: "auth",
        summary: "Delete a user",
        request: None,
        response: None,
        status: 204,
    },
    Route {
        path: "/api/v1/tokens",
        method: HttpMethod::Get,
        tag: "auth",
        summary: "List API tokens",
        request: None,
        response: Some("TokenView"),
        status: 200,
    },
    Route {
        path: "/api/v1/tokens",
        method: HttpMethod::Post,
        tag: "auth",
        summary: "Create an API token",
        request: Some("CreateToken"),
        response: Some("CreatedToken"),
        status: 201,
    },
    Route {
        path: "/api/v1/tokens/{id}",
        method: HttpMethod::Delete,
        tag: "auth",
        summary: "Revoke an API token",
        request: None,
        response: None,
        status: 204,
    },
    Route {
        path: "/api/v1/audit",
        method: HttpMethod::Get,
        tag: "auth",
        summary: "Recent audit events",
        request: None,
        response: Some("AuditView"),
        status: 200,
    },
];

fn add(doc: &mut OpenApi, route: &Route) {
    use utoipa::openapi::content::ContentBuilder;
    use utoipa::openapi::path::OperationBuilder;
    use utoipa::openapi::request_body::RequestBodyBuilder;
    use utoipa::openapi::response::{ResponseBuilder, ResponsesBuilder};

    let mut responses = ResponsesBuilder::new();
    let response_body = ResponseBuilder::new().description(route.summary);
    let response_body = if let Some(name) = route.response {
        response_body.content(
            "application/json",
            ContentBuilder::new()
                .schema(Some(Ref::from_schema_name(name)))
                .build(),
        )
    } else {
        response_body
    };
    responses = responses.response(route.status.to_string(), response_body.build());
    let mut operation = OperationBuilder::new()
        .tag(route.tag)
        .summary(Some(route.summary))
        .operation_id(Some(format!(
            "{}_{}",
            method_name(&route.method),
            route
                .path
                .trim_start_matches("/api/")
                .replace(['/', '{', '}'], "_")
        )))
        .responses(responses.build());
    if let Some(name) = route.request {
        operation = operation.request_body(Some(
            RequestBodyBuilder::new()
                .content(
                    "application/json",
                    ContentBuilder::new()
                        .schema(Some(Ref::from_schema_name(name)))
                        .build(),
                )
                .build(),
        ));
    }
    doc.paths
        .add_path_operation(route.path, vec![route.method.clone()], operation.build());
}

fn method_name(method: &HttpMethod) -> &'static str {
    match method {
        HttpMethod::Get => "get",
        HttpMethod::Post => "post",
        HttpMethod::Put => "put",
        HttpMethod::Patch => "patch",
        HttpMethod::Delete => "delete",
        HttpMethod::Options => "options",
        HttpMethod::Head => "head",
        HttpMethod::Trace => "trace",
    }
}
