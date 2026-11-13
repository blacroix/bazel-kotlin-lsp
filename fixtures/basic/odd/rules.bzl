def _odd_impl(ctx):
    out = ctx.actions.declare_file(ctx.label.name + ".txt")
    ctx.actions.write(out, "\n".join(ctx.attr.deps))
    return [DefaultInfo(files = depset([out]))]

odd_rule = rule(
    implementation = _odd_impl,
    attrs = {
        "deps": attr.string_list(),
        "exports": attr.label(allow_files = True),
    },
)
