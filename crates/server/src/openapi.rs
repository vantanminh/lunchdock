//! OpenAPI document served at `/api/openapi.json` and rendered at `/api/docs`.

use utoipa::openapi::path::HttpMethod;
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::openapi::{OpenApi, Ref};
use utoipa::{Modify, OpenApi as OpenApiDerive};

use crate::deploy::DeploymentView;
use crate::routes::account::{
    AuditView, CreateToken, CreateUser, CreatedToken, LoginRequest, PasswordChange,
    SessionResponse, SetupRequest, SetupStatus, TokenView, TotpDisable, TotpEnable,
    TotpSetupResponse, UpdateUser, UserView,
};
use crate::routes::catalog::{
    CreateEnvironment, CreateProject, CreateServer, CreateService, EnvVarView, EnvironmentView,
    ProjectView, PutEnvVar, ServerView, ServiceView, UpdateServer, UpdateService,
};
use crate::routes::edge::{
    CertificateView, CreateDomain, CreateRegistry, DeployBody, DomainView, PanelView,
    PreviewDomain, RegistryView, RollbackBody, UpdateDomain, UpdatePanel, UploadCertificate,
};
use crate::routes::git::{
    BuildAccepted, BuildQueueView, CreateGitProvider, GitProviderView, SourceView, UpdateSource,
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
        AuditView,
        ServerView,
        CreateServer,
        UpdateServer,
        ProjectView,
        CreateProject,
        EnvironmentView,
        CreateEnvironment,
        ServiceView,
        CreateService,
        UpdateService,
        EnvVarView,
        PutEnvVar,
        CertificateView,
        UploadCertificate,
        DomainView,
        CreateDomain,
        UpdateDomain,
        PreviewDomain,
        RegistryView,
        CreateRegistry,
        DeployBody,
        RollbackBody,
        DeploymentView,
        PanelView,
        UpdatePanel,
        GitProviderView,
        CreateGitProvider,
        SourceView,
        UpdateSource,
        BuildAccepted,
        BuildQueueView
    )),
    modifiers(&SecurityAddon),
    tags(
        (name = "system", description = "Process health"),
        (name = "auth", description = "Setup, sessions, users, tokens, and audit"),
        (name = "catalog", description = "Servers, projects, environments, and services"),
        (name = "edge", description = "Domains, certificates, registries, and image deploys"),
        (name = "source", description = "Git providers, webhooks, and builds")
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
    Route {
        path: "/api/v1/servers",
        method: HttpMethod::Get,
        tag: "catalog",
        summary: "List servers",
        request: None,
        response: Some("ServerView"),
        status: 200,
    },
    Route {
        path: "/api/v1/servers",
        method: HttpMethod::Post,
        tag: "catalog",
        summary: "Add a server",
        request: Some("CreateServer"),
        response: Some("ServerView"),
        status: 201,
    },
    Route {
        path: "/api/v1/projects",
        method: HttpMethod::Post,
        tag: "catalog",
        summary: "Create a project",
        request: Some("CreateProject"),
        response: Some("ProjectView"),
        status: 201,
    },
    Route {
        path: "/api/v1/environments/{id}/services",
        method: HttpMethod::Post,
        tag: "catalog",
        summary: "Create a service",
        request: Some("CreateService"),
        response: Some("ServiceView"),
        status: 201,
    },
    Route {
        path: "/api/v1/services/{id}/env",
        method: HttpMethod::Put,
        tag: "catalog",
        summary: "Set an environment variable",
        request: Some("PutEnvVar"),
        response: None,
        status: 204,
    },
    Route {
        path: "/api/v1/certificates",
        method: HttpMethod::Post,
        tag: "edge",
        summary: "Upload a certificate",
        request: Some("UploadCertificate"),
        response: Some("CertificateView"),
        status: 201,
    },
    Route {
        path: "/api/v1/certificates/match",
        method: HttpMethod::Get,
        tag: "edge",
        summary: "Find the certificate that matches a host",
        request: None,
        response: Some("CertificateView"),
        status: 200,
    },
    Route {
        path: "/api/v1/services/{id}/domains",
        method: HttpMethod::Post,
        tag: "edge",
        summary: "Attach a domain",
        request: Some("CreateDomain"),
        response: Some("DomainView"),
        status: 201,
    },
    Route {
        path: "/api/v1/registries",
        method: HttpMethod::Post,
        tag: "edge",
        summary: "Save registry credentials",
        request: Some("CreateRegistry"),
        response: Some("RegistryView"),
        status: 201,
    },
    Route {
        path: "/api/v1/services/{id}/deploy",
        method: HttpMethod::Post,
        tag: "edge",
        summary: "Deploy a container image",
        request: Some("DeployBody"),
        response: Some("DeploymentView"),
        status: 200,
    },
    Route {
        path: "/api/v1/services/{id}/rollback",
        method: HttpMethod::Post,
        tag: "edge",
        summary: "Roll back to a successful deployment",
        request: Some("RollbackBody"),
        response: Some("DeploymentView"),
        status: 200,
    },
    Route {
        path: "/api/v1/panel",
        method: HttpMethod::Patch,
        tag: "edge",
        summary: "Set the panel domain",
        request: Some("UpdatePanel"),
        response: Some("PanelView"),
        status: 200,
    },
    Route {
        path: "/api/v1/git-providers",
        method: HttpMethod::Post,
        tag: "source",
        summary: "Save a git provider",
        request: Some("CreateGitProvider"),
        response: Some("GitProviderView"),
        status: 201,
    },
    Route {
        path: "/api/v1/services/{id}/source",
        method: HttpMethod::Put,
        tag: "source",
        summary: "Set the git source",
        request: Some("UpdateSource"),
        response: Some("SourceView"),
        status: 200,
    },
    Route {
        path: "/api/v1/services/{id}/build",
        method: HttpMethod::Post,
        tag: "source",
        summary: "Queue a git build",
        request: None,
        response: Some("BuildAccepted"),
        status: 202,
    },
    Route {
        path: "/api/v1/services/{id}/webhook",
        method: HttpMethod::Post,
        tag: "source",
        summary: "Accept a git push webhook",
        request: None,
        response: None,
        status: 202,
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
