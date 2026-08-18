wasmtime::component::bindgen!({
    path: "../next-plugin-api/wit",
    world: "plugin",
    imports: { default: async | trappable },
    exports: { default: async },
});
