-- Compose stacks route a domain to one service inside the stack.
ALTER TABLE domains ADD COLUMN compose_service TEXT;
