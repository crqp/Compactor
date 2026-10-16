fn main() -> Result<(), Box<dyn std::error::Error>> {
    let build = vergen_gitcl::Build::all_build();
    let gitcl = vergen_gitcl::Gitcl::all_git();

    vergen_gitcl::Emitter::default()
        .add_instructions(&build)?
        .add_instructions(&gitcl)?
        .emit()?;

    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("compact.ico");
        res.compile()?;
    }

    Ok(())
}
