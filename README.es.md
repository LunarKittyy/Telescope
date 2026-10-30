<p align="center"><img src="docs/assets/banner.png" alt="Telescope: usa tu teléfono como cámara web, con todas sus lentes y control manual completo. La app de escritorio muestra el video de un teléfono, con la app del teléfono al lado." width="720"></p>

<p align="center"><a href="README.md">English</a> · Español · <a href="README.ru.md">Русский</a></p>

<p align="center">
  <a href="../../releases/latest"><img src="https://img.shields.io/endpoint?url=https://telescope.webcam/release.json" alt="Versión"></a>
  <a href="../../releases"><img src="https://img.shields.io/endpoint?url=https://telescope.webcam/downloads.json" alt="Descargas"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-AGPL--3.0-orange" alt="Licencia"></a>
  <a href="https://ko-fi.com/lunarkittyy"><img src="https://img.shields.io/badge/Ko--fi-support-FF5E5B?logo=ko-fi&logoColor=white" alt="Ko-fi"></a>
</p>

<p align="center">Telescope convierte tu teléfono Android en una cámara web para tu computadora con Linux o Windows.<br>Puedes usar todas sus lentes, desde el ultra gran angular hasta el teleobjetivo. Es gratis, sin marcas de agua ni anuncios y sin necesidad de crear una cuenta.</p>

<p align="center">
  <a href="https://github.com/LunarKittyy/Telescope/releases/latest/download/TelescopeSetup.exe"><img src="docs/assets/download-windows.svg" alt="Descargar para Windows" height="56"></a>
  <a href="https://github.com/LunarKittyy/Telescope/releases/latest/download/Telescope-linux.tar.gz"><img src="docs/assets/download-linux.svg" alt="Descargar para Linux" height="56"></a>
  <a href="https://github.com/LunarKittyy/Telescope/releases/latest/download/Telescope.apk"><img src="docs/assets/download-android.svg" alt="Descargar para Android" height="56"></a>
</p>

## Todo listo en unos dos minutos

<p align="center"><img src="site/media/setup.avif" alt="La lista de pasos de la app de escritorio, un teléfono escaneando el código de vinculación y Telescope seleccionado como cámara en una videollamada" width="800"></p>

1. **Descarga la app de escritorio.** En Windows ejecuta `TelescopeSetup.exe` (se instala solo para tu usuario y no necesita permisos de administrador). En Linux descomprime el archivo y ejecuta `./start.sh`.
2. **Sigue la lista de pasos que aparece al abrirla.** Te guía para configurar la cámara virtual, instalar la app en tu teléfono mediante un código QR y vincular ambas apps.
3. **Elige la cámara en tu app.** En Discord, Zoom, OBS o cualquier otra app que use una cámara web, la encontrarás como **Telescope** en Windows y **Phone Camera** en Linux.

<details>
<summary>Qué necesita Linux</summary>

- **Python 3.12 o posterior**
- **`v4l2loopback`** de tu distribución, para la cámara virtual (`sudo apt install v4l2loopback-dkms` en Debian y Ubuntu, `sudo dnf install v4l2loopback` en Fedora con [RPM Fusion](https://rpmfusion.org/))
- **`adb`** solo si quieres usar USB. Por Wi-Fi no hace falta

La [guía de configuración manual](docs/setup.md#linux) explica el resto, incluido qué hacer con la versión antigua de Python que trae Ubuntu 22.04.
</details>

¿No tienes un teléfono Android? La [cámara del navegador](docs/features.md#browser-camera) transmite desde un iPhone, una tablet o una laptop vieja directamente desde su navegador, sin instalar nada.

## Más que una cámara web

Conecta el teléfono por cable o por Wi-Fi. Puedes usar hasta cuatro a la vez, cada uno como una cámara web independiente. También puedes usar el micrófono del teléfono.

<table>
  <tr>
    <td width="33%"><img src="site/media/pro.avif" alt="ISO, velocidad de obturación, enfoque y balance de blancos ajustados manualmente desde la app de escritorio"><br><b>Controles Pro</b><br>Control manual de ISO, velocidad de obturación, enfoque y balance de blancos, como en el modo Pro de la cámara de tu teléfono.</td>
    <td width="33%"><img src="site/media/framing.avif" alt="El encuadre automático mantiene a la persona dentro del encuadre"><br><b>Encuadre automático</b><br>Ajusta el zoom y mueve la imagen para mantenerte dentro del encuadre.</td>
    <td width="33%"><img src="site/media/chroma.avif" alt="Una pantalla verde sustituida por una imagen"><br><b>Fondo y croma</b><br>Desenfoca el fondo o cámbialo, con o sin pantalla verde.</td>
  </tr>
  <tr>
    <td><img src="site/media/one-picture.avif" alt="Dos cámaras combinadas en una sola imagen"><br><b>Varias cámaras en una imagen</b><br>Una al lado de otra, en una cuadrícula o como tú las acomodes, para apps que solo aceptan una cámara.</td>
    <td><img src="site/media/desk.avif" alt="Una hoja de papel sobre un escritorio, con la perspectiva corregida para que se vea de frente"><br><b>Modo escritorio</b><br>Corrige la perspectiva de cualquier objeto plano que enfoques con el teléfono, como una página o un dibujo, para que se vea de frente en las llamadas.</td>
    <td><img src="site/media/fun.avif" alt="Efectos retro como VHS y Game Boy en la imagen de la cámara"><br><b>Paquete de diversión</b><br>Fiesta de clones, una capa de invisibilidad, VHS, una cámara web de 1998 y mucho más.</td>
  </tr>
</table>

La mayoría de estas funciones son complementos que activas cuando los necesitas. También hay gradación de color, desenfoque de privacidad, una etiqueta de nombre, notas junto a la lente y otros complementos. Puedes instalar también complementos de otras personas.

[Todo lo que puede hacer Telescope](docs/features.md) está en la documentación, junto con [una comparación](https://telescope.webcam/compare.html) con DroidCam, Iriun y Camo.

## Preguntas frecuentes

<details>
<summary><b>¿De verdad es gratis?</b></summary>

Sí. Todas las funciones, todas las resoluciones, sin marcas de agua ni anuncios. Si aun así quieres aportar algo, puedes hacerlo en [Ko-fi](https://ko-fi.com/lunarkittyy).
</details>

<details>
<summary><b>¿Mi video pasa por el servidor de alguien?</b></summary>

No. Va directamente del teléfono a tu computadora, por USB o por tu propia red, cifrado con TLS. No hay cuentas ni nube. Las apps solo se conectan a Internet para buscar actualizaciones en GitHub o para descargar adb de Google o VB-Cable de VB-Audio cuando se lo pides. Úsalo solo en una red de confianza o activa **Solo local - USB** en la app de Android. Más detalles en [Privacidad](docs/features.md#privacy).
</details>

<details>
<summary><b>¿Funciona con Discord / Zoom / Teams / OBS / mi navegador?</b></summary>

Si la app o el navegador permite usar una cámara web, sí. Telescope aparece como una cámara normal y la eliges como cualquier otra.
</details>

<details>
<summary><b>Tengo un iPhone o una Mac.</b></summary>

La app del teléfono es solo para Android, pero la [cámara del navegador](docs/features.md#browser-camera) transmite desde el navegador de un iPhone sin instalar nada. La app de escritorio funciona en Windows y Linux, no en macOS.
</details>

<details>
<summary><b>¿Funciona en mi teléfono?</b></summary>

Necesita Android 8 o posterior. En [Compatibilidad de dispositivos](docs/device-compatibility.md) puedes ver qué modelos se han probado. Si lo pruebas en un teléfono que no aparece ahí, [cuéntame cómo te fue](https://github.com/LunarKittyy/Telescope/discussions/new?category=device-compatibility). Es una de las cosas más útiles que puedes hacer por el proyecto.
</details>

<details>
<summary><b>¿Prefieres no instalar nada en Windows?</b></summary>

`Telescope-windows-portable.zip` en la [página de versiones](../../releases) contiene la misma app. Descomprime el archivo y ejecuta `TelescopeDesktop.exe` sin moverlo de su carpeta, porque la carpeta `lib-...` que tiene al lado también forma parte de la app.
</details>

<details>
<summary><b>¿Está hecho con IA?</b></summary>

Sí, uso agentes de IA para gran parte del proyecto. El código se genera con IA, pero yo decido qué hace Telescope y cómo debe comportarse. Las pruebas en hardware real también las hago principalmente yo (sin contar las pruebas automatizadas).
</details>

¿Algo no funciona o tienes una idea? Pregunta en [Discussions](../../discussions). ¿Encontraste un error? Abre un [reporte](https://github.com/LunarKittyy/Telescope/issues/new?template=bug.yml) y adjunta el informe que obtienes con **Copiar diagnóstico**. La [guía de solución de problemas](docs/troubleshooting.md) cubre los problemas más comunes.

## Cómo ayudar

Son bienvenidos los reportes sobre dispositivos, los reportes de errores con diagnóstico y las pull requests. [CONTRIBUTING.md](CONTRIBUTING.md) tiene los detalles y [la documentación](docs/README.md) explica cómo está hecho.

Telescope se distribuye bajo la licencia [GNU Affero General Public License v3.0](LICENSE). En [Licencias](docs/licenses.md) se explica qué significa para ti y qué componentes de terceros usa.

<a href="https://www.star-history.com/?repos=LunarKittyy%2FTelescope&type=date&releases=&legend=top-left">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/chart?repos=LunarKittyy/Telescope&type=date&theme=dark&releases=&legend=top-left" />
    <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/chart?repos=LunarKittyy/Telescope&type=date&releases=&legend=top-left" />
    <img alt="Gráfico del historial de estrellas" src="https://api.star-history.com/chart?repos=LunarKittyy/Telescope&type=date&releases=&legend=top-left" />
  </picture>
</a>
