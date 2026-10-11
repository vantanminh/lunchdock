export type Schemas = import("./api-types").components["schemas"];

export type Role = "admin" | "viewer";

export type User = Schemas["UserView"];
export type SessionResponse = Schemas["SessionResponse"];
export type TokenView = Schemas["TokenView"];
export type CreatedToken = Schemas["CreatedToken"];
export type AuditEvent = Schemas["AuditView"];
