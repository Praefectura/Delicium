# Delicium

[English](README.md) · [简体中文](README.zh-CN.md) · [Latine](README.la.md)

*Delicium* nomen Latinum est, quod "delicias" vel "rem caram" significat: singulariter
*delicium*, pluraliter *deliciae*. Itaque vocabulum ipsum et unum animalculum et totam
tabulam operis designat.

Delicium est programma Windows quod personas animatas in tabula operis ostendit. Fenestra
perlucida in tabula operis manet, et personae formis propriis Delicii utuntur: formae duae
dimensionis parametris et formae trium dimensionis ossium. Machina ludi universalis non
adhibetur; wgpu, winit, egui, bevy_ecs infra sunt. Primum solum Windows; celeritas et
species picturae ante omnia, non convenientia cum formis alienis.

**Quid facere potest:**

- Fenestra perlucida: sola persona murem accipit, campus vacuus murem transit. Fenestra
  autem imperii semper murem accipit.
- Formae 2D parametris (min / max / default), rotatio et distortio, pictura, masca
  polygonorum.
- Formae 3D cum ossibus, animatio, cutis per GPU, materia asperitatis et metalli, lux
  directa, profunditas.
- Simulatio fixa 60 Hz, gradus quattuor per imaginem, interpolatio in reddendo.
- Usque ad 64 personas; vicinae intra spatium 2.5 unitatum inter se respondent.
- Personae sponte ambulant in campo visibili, nisi `--no-wander`.
- Ictus muris: preme et solve in eodem loco (minus quam 4 px) — persona se vertit et
  saltat per circa unum secundum; motus est in pictura tantum, locus non mutatur.
- Icona in area nuntiorum: sinistra preme ut fenestram imperii ostendas aut celes,
  dextra preme ut menu aperias.
- Configuratio in `%APPDATA%\Delicium\config.json` servatur et in initio legitur: personae,
  loci, mores (ictus regionum, vagatio), lingua, persona prima, et finis colloquii.
- **Bibliotheca formarum**: machina nulli formae certae alligata est. Quinque formae
  inclusae per id vocantur (`bangboo-eous`, `puppet2d`, `robot3d`, et varietates pictae);
  quodlibet `*.dmodel.json` in `%APPDATA%\Delicium\models\` positum fit pars bibliothecae.
- **Lingua duplex**: Sinica simplificata et Anglica, in pagina praeferentiarum electa aut ex
  lingua Windows sumpta.
- **Fenestra colloquii**: unaquaeque persona memoriam colloquii in fenestra imperii habet.
  Sine fine constituto lineis localibus respondet; cum fine OpenAI-compatibili nuntium,
  contextum et responsum mittit et ostendit. Colloquia inter personas eodem in indice
  scribuntur.
- Unum animalculum Bangboo (Eous) in `--bangboo` modo (idem ac `--pet bangboo-eous`);
  alterum in fenestra imperii e bibliotheca additur; formae propriae exempli etiam adsunt.

**Aedificare et agere** (Rust MSVC in Windows):

```powershell
& "$HOME/.cargo/bin/cargo.exe" run --release
& "$HOME/.cargo/bin/cargo.exe" run --release -- --bangboo
```

Programma stabile `delicium-launcher.exe` primum se ipsum et machinam in
`%LOCALAPPDATA%\Delicium\` ponit, deinde novam versionem ex GitHub quaerit, et animalculum
aperit. In initio nulla fenestra apparet.

Plena praecepta: [docs/USAGE.md](docs/USAGE.md) (Sinice). Formae descriptio:
[docs/MODEL-FORMAT.md](docs/MODEL-FORMAT.md). Examinationes:
[docs/VERIFICATION.md](docs/VERIFICATION.md).

---

## Documentation

| File | Contents | Language |
|---|---|---|
| [docs/USAGE.md](docs/USAGE.md) | Build, run, every command-line flag, control window, overlay and multi-monitor behaviour | 中文 |
| [docs/MODEL-FORMAT.md](docs/MODEL-FORMAT.md) | Complete `delicium.model` specification, budgets, texture rules | 中文 |
| [docs/VERIFICATION.md](docs/VERIFICATION.md) | Test procedures and every dated acceptance record, including failed gates | 中文 |
| [docs/ROADMAP.md](docs/ROADMAP.md) | Current status, what is out of scope, licence table | 中文 |
| [assets/examples/bangboo-eous.md](assets/examples/bangboo-eous.md) | Eous asset provenance, CC BY 4.0 attribution, modifications | 中文 |

The documentation files are Chinese because they were written during development and are
moved verbatim so that no measured number is retyped. Contributions that add English or
Latin versions of them are welcome.

Delicium's measured numbers in `docs/VERIFICATION.md` are per-run results from the day
they were recorded. A later improvement does not turn a failed gate into a pass, and the
file keeps both.
