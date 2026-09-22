pub type AccountLinkInteractionResource = std::sync::Arc<dyn crate::AccountLinkInteraction>;

wasmtime::component::bindgen!({
    path: "../next-plugin-api/wit",
    world: "storefront",
    imports: { default: async | trappable },
    exports: { default: async },
    with: {
        "bottles:plugin/account-link.interaction": AccountLinkInteractionResource,
    },
});
